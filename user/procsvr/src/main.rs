#![no_std]
#![no_main]
#![feature(maybe_uninit_slice, panic_info_message)]

use core::{arch::global_asm, num::NonZeroU64};
use core::{cmp, slice};

extern crate alloc;

#[allow(unused_macros)]
#[macro_export]
macro_rules! println {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut $crate::DebugPrint, format_args!($($tt)*)).unwrap();
	core::fmt::write(&mut $crate::DebugPrint, format_args!("\n")).unwrap();
    }}
}

#[allow(unused_macros)]
#[macro_export]
macro_rules! print {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut $crate::DebugPrint, format_args!($($tt)*)).unwrap();
    }}
}

mod init;

use alloc::vec::Vec;
use alloc::{collections::BTreeMap, vec};
use init::{Init, InitState};
use rille::elf::Segment;
use rille::prelude::*;
use rille::{
    addr::VirtualConst,
    capability::{
        paging::{BasePage, MegaPage, Page, PageTable, PageTableFlags},
        AnnotatedCaptr, Capability, Endpoint, Job, Notification, Thread,
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
    let _thread = Thread::from_captr(thread.cap);
    let _stack_pg1 = Page::<MegaPage>::from_captr(stack_pg1.cap);
    let _stack_pg2 = Page::<MegaPage>::from_captr(stack_pg2.cap);
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
    let _root_job = Job::from_captr(root_job.cap);

    let mut image_pages = vec![AnnotatedCaptr::default(); n_pages];
    for pg in image_pages.iter_mut() {
        ep.recv_with_regs(None, Some(pg)).unwrap();
    }

    let mut sender = None;
    let mut init_sm = Init {
        state: InitState::Nop,
        stack: Vec::new(),
    };

    loop {
        let (hdr, regs) = ep.recv_with_regs(Some(&mut sender), None).unwrap();

        match sender.map(NonZeroU64::get).unwrap_or_default() {
            0 => {
                // TODO
            }
            0x1 => {
                init_sm.step(hdr, regs, ep, job, init_thread, pgtbl);
            }
            _pid => {
                // TODO
            }
        }
    }
}

#[derive(Debug)]
pub struct Process {
    pub job: Job,
    pub thread: Thread,
    pub pgtbl: PageTable,
    pub ipc_buf: Page<BasePage>,
    pub pages: Vec<Page<BasePage>>,
    pub elf: Elf,
    pub phdrs: Vec<Segment>,
    pub stack: Page<MegaPage>,
}

#[derive(Debug, Default)]
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
pub fn copy_to_ipc(s: &[u8]) {
    unsafe { slice::from_raw_parts_mut(0xA000_0000 as *mut u8, cmp::min(s.len(), 4096)) }
        .copy_from_slice(s);
}

pub fn copy_from_ipc(s: &mut [u8]) {
    s.copy_from_slice(unsafe {
        slice::from_raw_parts(0xA000_0000 as *const u8, cmp::min(s.len(), 4096))
    });
}
