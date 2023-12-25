#![no_std]
#![no_main]
#![feature(maybe_uninit_slice, panic_info_message)]

use core::{arch::global_asm, mem::MaybeUninit, num::NonZeroU64};
use core::{cmp, ptr, slice};

extern crate alloc;

#[allow(unused_macros)]
macro_rules! println {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrint, format_args!($($tt)*)).unwrap();
	core::fmt::write(&mut DebugPrint, format_args!("\n")).unwrap();
    }}
}

#[allow(unused_macros)]
macro_rules! print {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrint, format_args!($($tt)*)).unwrap();
    }}
}

use alloc::vec::Vec;
use alloc::{collections::BTreeMap, vec};
use rille::addr::{Identity, Vpn};
use rille::elf::{Section, Segment, SegmentFlags, SegmentType};
use rille::prelude::*;
use rille::{
    addr::VirtualConst,
    capability::{
        paging::{BasePage, MegaPage, Page, PageTable, PageTableFlags},
        AnnotatedCaptr, Capability, Captr, CaptrOps, Endpoint, Job, MessageHeader, Notification,
        Thread,
    },
    elf::Elf,
    syscalls,
};
use rille_user::{malloc::LinkedListAlloc, sync::once_cell::OnceCell};

global_asm!(
    "
.pushsection .init
.option norvc
.type _start, @function
.global _start
_start:
.option push
.option norelax
    lla gp, __global_pointer$
.option pop

    lla t0, __bss_start
    lla t1, __bss_end
    // Clear the .bss section
clear_bss:
    beq t0, t1, done_clear_bss
    sd zero, (t0)
    addi t0, t0, 8
    j clear_bss

done_clear_bss:
    lla sp, __stack_top
    tail entry
.popsection
"
);

#[global_allocator]
static GLOBAL_ALLOC: OnceCell<LinkedListAlloc> = OnceCell::new();

#[no_mangle]
unsafe extern "C" fn entry(ep: Endpoint, n_pages: usize) -> ! {
    let mut job = AnnotatedCaptr::default();
    let mut thread = AnnotatedCaptr::default();
    let mut stack_pg1 = AnnotatedCaptr::default();
    let mut stack_pg2 = AnnotatedCaptr::default();
    let mut ipc_buf = AnnotatedCaptr::default();
    let mut pgtbl = AnnotatedCaptr::default();

    ep.recv_with_regs(None, Some(&mut job)).unwrap();
    ep.recv_with_regs(None, Some(&mut thread)).unwrap();
    ep.recv_with_regs(None, Some(&mut stack_pg1)).unwrap();
    ep.recv_with_regs(None, Some(&mut stack_pg2)).unwrap();
    ep.recv_with_regs(None, Some(&mut ipc_buf)).unwrap();
    ep.recv_with_regs(None, Some(&mut pgtbl)).unwrap();

    let job = Job::from_captr(job.cap);
    let thread = Thread::from_captr(thread.cap);
    let stack_pg1 = Page::<MegaPage>::from_captr(stack_pg1.cap);
    let stack_pg2 = Page::<MegaPage>::from_captr(stack_pg2.cap);
    let ipc_buf = Page::<BasePage>::from_captr(ipc_buf.cap);
    let pgtbl = PageTable::from_captr(pgtbl.cap);

    ipc_buf
        .map(
            pgtbl,
            VirtualConst::from_usize(0xA000_0000),
            PageTableFlags::RW,
        )
        .unwrap();

    // SAFETY: The provided base address is valid and can be mapped.
    unsafe {
        GLOBAL_ALLOC
            .get_or_init(|| LinkedListAlloc::new(Notification::create().unwrap(), pgtbl))
            .init(0xF000_0000 as *mut u8);
    }

    let mut init_thread = AnnotatedCaptr::default();
    let mut root_job = AnnotatedCaptr::default();

    ep.recv_with_regs(None, Some(&mut init_thread)).unwrap();
    ep.recv_with_regs(None, Some(&mut root_job)).unwrap();

    let init_thread = Thread::from_captr(init_thread.cap);
    let root_job = Job::from_captr(root_job.cap);

    let mut image_pages = vec![AnnotatedCaptr::default(); n_pages];
    for pg in image_pages.iter_mut() {
        ep.recv_with_regs(None, Some(pg)).unwrap();
    }

    let mut sender = None;
    let mut init_data = InitData::default();
    loop {
        let (hdr, regs) = ep.recv_with_regs(Some(&mut sender), None).unwrap();

        match sender.map(NonZeroU64::get).unwrap_or_default() {
            0 => {
                // TODO
            }
            0x1 => {
                handle_init_calls(hdr, regs, ep, job, init_thread, pgtbl, &mut init_data);
            }
            pid => {
                // TODO
            }
        }
    }
}

#[derive(Default)]
struct InitData {
    purpose: u64,
    n_pages: u64,
    page_req: usize,
    console: Option<Elf>,
    console_job: Option<Job>,
    console_thread: Option<Thread>,
    console_pgtbl: Option<PageTable>,
    console_ipc_buf: Option<Page<BasePage>>,
    console_pages: Vec<Page<BasePage>>,
    console_phdrs: Vec<Segment>,
    console_shdrs: Vec<Section>,
    console_stack: Option<Page<MegaPage>>,
    segments_loaded: Vec<Segment>,
    segments_to_load: Vec<Segment>,
    data: SegmentedCursor,
}

// struct Process {
//     job: Job,
//     thread: Thread,
//     pgtbl: PageTable,
//     ipc_buf: Page<BasePage>,
//     pages: Vec<Page<BasePage>>,
//     elf: Elf,
//     phdrs: Vec<Segment>,
// }

fn handle_init_calls(
    hdr: MessageHeader,
    regs: [u64; 4],
    ep: Endpoint,
    job: Job,
    init_thread: Thread,
    pgtbl: PageTable,
    init: &mut InitData,
) {
    match hdr.private() {
        0x1 => {
            // Preamble: early process init
            init.purpose = regs[0];
            init.n_pages = regs[1];

            init.page_req = 0;
            // Request page 0
            ep.reply_with_regs(
                MessageHeader::new().with_length(1).with_private(0x1),
                Captr::null(),
                [0; 4],
            )
            .unwrap();
        }
        0x2 => {
            // Given requested page.
            init.data.inner.insert(init.page_req, [0; 4096]);
            copy_from_ipc(init.data.inner.get_mut(&init.page_req).unwrap());

            if init.console.is_none() {
                // Try to parse the ELF.
                let Ok(elf) = Elf::parse_header(&mut init.data) else {
                    init_request_page(ep, init);
                    return;
                };
                init.console = Some(elf);
            }
            match &mut init.console {
                None => unreachable!(),
                Some(elf) => {
                    if init.console_phdrs.len() != elf.n_segments() as usize {
                        let Ok(mut iter) = elf.parse_segments(&mut init.data) else {
                            init_request_page(ep, init);
                            return;
                        };
                        let len = init.console_phdrs.len();
                        for (ix, segment) in (&mut iter).enumerate() {
                            if ix < len {
                                continue;
                            }
                            let Ok(seg) = segment else {
                                drop(iter);
                                init_request_page(ep, init);
                                return;
                            };
                            init.console_phdrs.push(seg);
                        }
                    }
                    // if init.console_shdrs.len() != elf.n_sections() as usize {
                    //     let Ok(mut iter) = elf.parse_sections(&mut init.data) else {
                    //         init_request_page(ep, init);
                    //         return;
                    //     };
                    //     let len = init.console_shdrs.len();
                    //     for (ix, section) in (&mut iter).enumerate() {
                    //         if ix < len {
                    //             continue;
                    //         }
                    //         let Ok(sec) = section else {
                    //             drop(iter);
                    //             init_request_page(ep, init);
                    //             return;
                    //         };
                    //         init.console_shdrs.push(sec);
                    //     }
                    // }

                    if init.console_phdrs.len() == elf.n_segments() as usize
                        && init.console_job.is_none()
                    {
                        init.console_job = Some(Job::create(job).unwrap());
                        init.console_thread = Some(
                            init.console_job
                                .unwrap()
                                .create_thread("console-driver")
                                .unwrap(),
                        );
                        init.console_pgtbl = Some(PageTable::create().unwrap());
                        init.console_ipc_buf = Some(Page::create().unwrap());
                        init.console_stack = Some(Page::create().unwrap());
                        unsafe {
                            init.console_thread
                                .unwrap()
                                .configure(
                                    init.console_pgtbl.unwrap(),
                                    init.console_ipc_buf.unwrap(),
                                )
                                .unwrap();
                        }
                        init.console_stack
                            .unwrap()
                            .map(
                                init.console_pgtbl.unwrap(),
                                VirtualConst::from_usize(0x3fffe00000),
                                PageTableFlags::RW,
                            )
                            .unwrap();
                        init.segments_to_load = init
                            .console_phdrs
                            .iter()
                            .copied()
                            .filter(|x| x.seg_type == SegmentType::Load)
                            .collect();
                    }

                    if init.console_job.is_some() && !init.segments_to_load.is_empty() {
                        while let Some(seg) = init.segments_to_load.pop() {
                            if seg.align != 4096 {
                                todo!();
                            }
                            let base_addr = seg.virt_addr.next_multiple_of(seg.align) - seg.align;

                            // TODO: opt this so we can keep it over iterations
                            let mut file_data = vec![0; seg.file_size];
                            init.data.seek(SeekFrom::Start(seg.offset as u64)).unwrap();
                            let Ok(()) = init.data.read_exact(&mut file_data) else {
                                init.segments_to_load.push(seg);
                                init_request_page(ep, init);
                                return;
                            };

                            let n_pages = ((seg.virt_addr + seg.mem_size).next_multiple_of(4096)
                                - base_addr)
                                / 4096;

                            let pgs = (0..n_pages)
                                .map(|_| Page::<BasePage>::create().unwrap())
                                .collect::<Vec<_>>();
                            if seg.file_size > 0 {
                                let mut addr = seg.virt_addr;
                                let mut sz = seg.file_size;
                                let mut data_offset = 0;
                                for (n, pg) in pgs.iter().copied().enumerate() {
                                    let off = addr - (base_addr + n * 4096);
                                    pg.map(
                                        pgtbl,
                                        VirtualConst::from_usize(0xE000_0000),
                                        PageTableFlags::RW,
                                    )
                                    .unwrap();

                                    let mut flags = PageTableFlags::empty();
                                    if seg.flags.contains(SegmentFlags::READ) {
                                        flags |= PageTableFlags::READ;
                                    }
                                    if seg.flags.contains(SegmentFlags::WRITE) {
                                        flags |= PageTableFlags::WRITE;
                                    }
                                    if seg.flags.contains(SegmentFlags::EXEC) {
                                        flags |= PageTableFlags::EXECUTE;
                                    }
                                    pg.map(
                                        init.console_pgtbl.unwrap(),
                                        VirtualConst::from_usize(base_addr + n * 4096),
                                        flags,
                                    )
                                    .unwrap();

                                    let len = cmp::min(sz, 4096 - off);

                                    // println!(
                                    //     "n={:?}, addr={:#x?}, off={:#x?}, len={:?}, sz={:?}, file_sz={:?}, mem_sz={:?}",
                                    //     n, addr, off, len, sz, seg.file_size, seg.mem_size
                                    // );
                                    unsafe {
                                        &mut slice::from_raw_parts_mut(0xE000_0000 as *mut _, 4096)
                                            [off..off + len]
                                    }
                                    .copy_from_slice(&file_data[data_offset..data_offset + len]);
                                    addr += len;
                                    sz -= len;
                                    data_offset += len;
                                }
                            }

                            init.segments_loaded.push(seg);
                        }
                    }

                    if init.segments_to_load.is_empty() {
                        unsafe {
                            init.console_thread
                                .unwrap()
                                .start(
                                    init.console.as_ref().unwrap().entry_addr.into(),
                                    (0x4000000000u64).into(),
                                    Captr::null(),
                                    0,
                                )
                                .unwrap();
                        }
                    }
                }
            }
        }
        0xdead => {
            // init is ready to die, kill it and delete it
            init_thread.suspend().unwrap();
            init_thread.delete().unwrap();
        }
        _ => todo!(),
    }
}

fn init_request_page(ep: Endpoint, init: &mut InitData) {
    let pg = init.data.pos / 4096;
    init.page_req = pg;
    ep.reply_with_regs(
        MessageHeader::new().with_length(1).with_private(0x1),
        Captr::null(),
        [pg as u64, 0, 0, 0],
    )
    .unwrap();
}

#[derive(Default)]
struct SegmentedCursor {
    inner: BTreeMap<usize, [u8; 4096]>,
    pos: usize,
}

impl Read for SegmentedCursor {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        let pg = self.pos / 4096;
        let len = buf.len();
        let pg = self.inner.get(&pg).ok_or(IoError)?;
        let pos = self.pos % 4096;
        let actual_len = cmp::min(pos + len, 4096) - pos;
        buf[0..actual_len].copy_from_slice(&pg[pos..cmp::min(pos + len, 4096)]);
        self.pos += actual_len;
        Ok(actual_len)
    }
}

impl Seek for SegmentedCursor {
    fn seek(&mut self, from: SeekFrom) -> Result<u64, IoError> {
        match from {
            SeekFrom::Start(x) => {
                self.pos = x as usize;
                Ok(x)
            }
            // TODO?
            SeekFrom::End(_) => Err(IoError),
            SeekFrom::Current(x) => {
                self.pos = self.pos.saturating_add_signed(x as isize);
                Ok(self.pos as u64)
            }
        }
    }
}

#[panic_handler]
unsafe fn panic_handler(panic: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(msg) = panic.message() {
        println!("panic occurred: {}", msg);
        if let Some(location) = panic.location() {
            println!(
                "  at {}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            );
        }
    }

    loop {
        core::arch::asm!("nop")
    }
}

struct DebugPrint;

impl core::fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        syscalls::debug::debug_print_string(s);
        Ok(())
    }
}

// TODO: smth like this in rille
fn copy_to_ipc(s: &[u8]) {
    unsafe { slice::from_raw_parts_mut(0xA000_0000 as *mut u8, cmp::min(s.len(), 4096)) }
        .copy_from_slice(s);
}

fn copy_from_ipc(s: &mut [u8]) {
    s.copy_from_slice(unsafe {
        slice::from_raw_parts(0xA000_0000 as *const u8, cmp::min(s.len(), 4096))
    });
}
