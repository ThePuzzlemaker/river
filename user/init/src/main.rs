#![no_std]
#![no_main]
#![feature(panic_info_message)]

use core::{
    arch::{asm, global_asm},
    cmp, fmt, mem,
    num::NonZeroU64,
    ptr, slice,
};

use alloc::vec;
use bootfs::{BootFsEntry, BootFsHeader};
use rille::{
    addr::{VirtualConst, VirtualMut},
    capability::paging::*,
    init::{BootInfo, InitCapabilities},
    prelude::*,
    syscalls::{ecall7, SyscallNumber},
};
use rille_user::{malloc::LinkedListAlloc, sync::once_cell::OnceCell};

extern crate alloc;
extern crate panic_handler;

#[allow(unused_macros)]
macro_rules! println {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrintBackup, format_args!($($tt)*)).unwrap();
	print_backup("\n");
    }}
}

#[allow(unused_macros)]
macro_rules! print {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrintBackup, format_args!($($tt)*)).unwrap();
    }}
}

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

// static CAPTR_ALLOC: OnceCell<CaptrAllocator<'static>> = OnceCell::new();

// extern "C" fn thread_entry(_thread: Thread, ep: usize) -> ! {
//     unsafe {
//         asm!("li tp, 1", options(nomem));
//     }

//     //    let caps = unsafe { InitCapabilities::new() };
//     // _thread.suspend().unwrap();
//     // caps.thread.suspend().unwrap();
//     // RemoteCaptr::remote(caps.captbl, caps.thread)
//     //     .delete()
//     //     .unwrap();
//     // syscalls::debug::debug_dump_root();

//     // syscalls::debug::debug_cap_slot(caps.captbl.into_raw(), caps.thread.into_raw()).unwrap();

//     // println!("\nthread 2 waiting to recv");
//     // println!("{:#x?}", unsafe {
//     //     Captr::from_raw_unchecked(65533).wait().unwrap()
//     // });

//     // loop {
//     //     let mut lock = SHARED2.expect("oops").lock();
//     //     let n = *lock;

//     //     if n >= 50_000 {
//     //         break;
//     //     }
//     //     *lock += 1;
//     //     print!("\rthread 2: {}       {:?}      ", n, time());
//     // }

//     println!("{:#?}", time());
//     loop {
//         let info = MessageHeader::from_raw(unsafe {
//             ecall3(SyscallNumber::EndpointRecv, ep as u64, 0, 0).unwrap()
//         });
//         if info.private() >= 100_000 {
//             break;
//         }
//         // unsafe {
//         //     ecall2(SyscallNumber::SaveCaller, 0, 61234).unwrap();
//         // }
//         // out_notif.signal().unwrap();

//         // while in_notif.wait().is_ok() {
//         //     let n = SHARED.fetch_add(1, Ordering::Relaxed);

//         //     //print!("\rpong!: {}       {:?}     ", n, time());

//         //     if n == 50_000 {
//         //         break;
//         //     }
//         //     out_notif.signal().unwrap()
//         // }

//         // let mut buf = [0; 4096];
//         // buf.copy_from_slice(unsafe { slice::from_raw_parts(0x6100_0000 as *const i8, 4096) });
//         // {
//         //     let lock = SHARED2.expect("oops").lock();
//         //     println!("Recv'd IPC: {info:x?}");
//         //     let s = unsafe { CStr::from_ptr(buf.as_ptr()) };
//         //     println!("{:#?}", s);
//         // }
//         // unsafe {
//         //     ptr::copy(
//         //         "Hello from thread 2!\0".as_ptr(),
//         //         0x6100_0000 as *mut _,
//         //         "Hello from thread 2!\0".len(),
//         //     );
//         // }

//         unsafe {
//             ecall7(
//                 SyscallNumber::EndpointReply,
//                 ep as u64,
//                 MessageHeader::new()
//                     .with_length(0)
//                     .with_private(info.private() + 1)
//                     .into_raw(),
//                 0,
//                 0,
//                 0,
//                 0,
//                 0,
//             )
//             .unwrap();
//         }
//     }
//     // unsafe { ecall2(SyscallNumber::Delete, 0, 61234).unwrap() };

//     loop {
//         unsafe {
//             core::arch::asm!("nop");
//         }
//     }

//     // loop {
//     //     unsafe {
//     //         core::arch::asm!("nop");
//     //     }
//     //     // print!("\rthread 2!");
//     // }
// }

// extern "C" fn thread3_entry(_thread: Thread, init_info: *const BootInfo, ep: Endpoint) -> ! {
//     let _init_info = unsafe { &*init_info };
//     let _init_caps = unsafe { InitCapabilities::new() };

//     for _ in 0..1_100_000 {
//         unsafe {
//             core::arch::asm!("pause");
//         }
//     }
//     // TODO: deal with length here

//     loop {
//         let mr0: u64;
//         let mr1: u64;
//         let mr2: u64;
//         let mr3: u64;
//         let err: u64;
//         let val: u64;
//         let hdr = unsafe {
//             core::arch::asm!(
//                         "ecall",
//                         in("a0") u64::from(SyscallNumber::EndpointRecv),
//                         in("a1") ep.into_raw() as u64,
//                         lateout("a0") err,
//             lateout("a1") val,
//             in("a2") 0,
//                 in("a3") 0,
//                         out("a4") mr0,
//                         out("a5") mr1,
//                         out("a6") mr2,
//                         out("a7") mr3
//                     );

//             if err != 0 {
//                 panic!("{:?}", CapError::from(err));
//             }

//             val
//         };
//         let buf = [mr0, mr1, mr2, mr3, 0];
//         let s = if MessageHeader::from_raw(hdr).length() <= 4 {
//             unsafe { CStr::from_ptr(buf.as_ptr().cast()) }
//         } else {
//             unsafe { CStr::from_ptr(0x6200_0000 as *const i8) }
//         };

//         // fmt::write(&mut DebugPrintBackup, format_args!("{s:?}\n")).unwrap();
//         let mut uart = UART_LOCK.expect("oops").lock();

//         for c in s.to_bytes() {
//             while uart.full() {
//                 // drop(uart);
//                 // core::hint::spin_loop();
//                 // uart = UART_LOCK.expect("oops").lock();
//                 drop(uart);
//                 UART_NOTIF.expect("oops").wait().unwrap();
//                 uart = UART_LOCK.expect("oops").lock();
//             }
//             uart.buf_push(*c);
//             uart.flush();
//         }
//         uart.flush();
//         drop(uart);

//         let s_len = s.to_bytes().len();
//         unsafe {
//             ptr::write(s.as_ptr().cast::<usize>().cast_mut(), s_len);
//         }

//         unsafe {
//             let _ = ecall3(
//                 SyscallNumber::EndpointReply,
//                 ep.into_raw() as u64,
//                 MessageHeader::new().with_length(1).into_raw(),
//                 0,
//             );
//         }
//     }
// }

// extern "C" fn thread2_entry(_thread: Thread, init_info: *const BootInfo) -> ! {
//     unsafe {
//         asm!("li tp, 2", options(nomem));
//     }
//     let init_info = unsafe { &*init_info };
//     let init_caps = unsafe { InitCapabilities::new() };
//     let notif = Notification::create().unwrap();
//     let lock_notif = Notification::create().unwrap();
//     let uart_notif = Notification::create().unwrap();

//     // unsafe {
//     //     ecall6(
//     //         SyscallNumber::CaptrGrant,
//     //         init_caps.captbl.into_raw() as u64,
//     //         notif.into_raw() as u64,
//     //         init_caps.captbl.into_raw() as u64,
//     //         notif.into_raw() as u64,
//     //         CapRights::all().bits(),
//     //         0xDEADBEEF,
//     //     )
//     //     .unwrap();
//     // }

//     // unsafe {
//     //     ecall6(
//     //         SyscallNumber::CaptrGrant,
//     //         init_caps.captbl.into_raw() as u64,
//     //         uart_notif.into_raw() as u64,
//     //         init_caps.captbl.into_raw() as u64,
//     //         uart_notif.into_raw() as u64,
//     //         CapRights::all().bits(),
//     //         0xDEADBEEF,
//     //     )
//     //     .unwrap();
//     // }

//     UART_LOCK.get_or_init(|| Mutex::new(UartInner::default(), lock_notif));
//     UART_NOTIF.get_or_init(|| uart_notif);

//     unsafe {
//         ecall3(
//             SyscallNumber::IntrPoolGet,
//             init_caps.intr_pool.into_raw() as u64,
//             init_info.free_slots.lo.add(2).into_raw() as u64,
//             0x0a,
//         )
//         .unwrap();
//     }
//     let intr_handler = init_info.free_slots.lo.add(2);

//     unsafe {
//         ecall2(
//             SyscallNumber::IntrHandlerBind,
//             intr_handler.into_raw() as u64,
//             notif.into_raw() as u64,
//         )
//         .unwrap();
//     }

//     // syscalls::debug::debug_dump_root();

//     while notif.wait().unwrap().is_some() {
//         unsafe {
//             ecall1(
//                 SyscallNumber::IntrHandlerAck,
//                 intr_handler.into_raw() as u64,
//             )
//             .unwrap();
//         }
//         // UART_LOCK.expect("oops").lock().flush();
//     }

//     loop {
//         unsafe {
//             core::arch::asm!("nop");
//         }
//     }
// }

// #[derive(Debug)]
// struct UartInner {
//     buf: [u8; 64],
//     tx_w: usize,
//     tx_r: usize,
// }

// impl Default for UartInner {
//     fn default() -> Self {
//         Self {
//             buf: [0; 64],
//             tx_w: 0,
//             tx_r: 0,
//         }
//     }
// }

// impl UartInner {
//     fn mask(i: usize) -> usize {
//         i & 63
//     }

//     #[inline]
//     fn buf_push(&mut self, c: u8) {
//         let i = Self::mask(self.tx_w);
//         self.buf[i] = c;
//         self.tx_w = self.tx_w.wrapping_add(1);
//     }

//     #[inline]
//     fn buf_shift(&mut self) -> u8 {
//         let i = Self::mask(self.tx_r);
//         let x = self.buf[i];
//         self.tx_r = self.tx_r.wrapping_add(1);
//         x
//     }

//     #[inline]
//     fn full(&self) -> bool {
//         self.size() == 64
//     }

//     #[inline]
//     fn empty(&self) -> bool {
//         self.tx_r == self.tx_w
//     }

//     #[inline]
//     fn size(&self) -> usize {
//         self.tx_w - self.tx_r
//     }

//     fn flush(&mut self) {
//         //fmt::write(&mut DebugPrintBackup, format_args!("{self:?}\n")).unwrap();
//         loop {
//             if self.empty() {
//                 // empty.
//                 return;
//             }

//             // LSR & LSR_TX_IDLE
//             if (unsafe { ptr::read_volatile(0x8000_0005 as *mut u8) } & (1 << 5)) == 0 {
//                 return;
//             }

//             let c = self.buf_shift();

//             // UART_NOTIF.expect("oops").signal().unwrap();

//             unsafe { ptr::write_volatile(0x8000_0000 as *mut _, c) }
//         }
//     }
// }

// unsafe impl Send for UartInner {}
// unsafe impl Sync for UartInner {}

// static UART_LOCK: OnceCell<Mutex<UartInner>> = OnceCell::new();
// static UART_NOTIF: OnceCell<Notification> = OnceCell::new();

// static SHARED2: OnceCell<Mutex<u64>> = OnceCell::new();

pub fn time() -> (u64, u64) {
    let timebase_freq = 10_000_000;
    let time: u64;

    unsafe { core::arch::asm!("csrr {}, time", out(reg) time, options(nostack)) };

    let time_ns = time * (1_000_000_000 / timebase_freq);
    let sec = time_ns / 1_000_000_000;
    let time_ns = time_ns % 1_000_000_000;
    let us = time_ns / 1_000;
    (sec, us)
}

static BOOTFS: &[u8] = include_bytes!(env!("CARGO_BUILD_BOOTFS_PATH"));

#[no_mangle]
extern "C" fn entry(init_info: *const BootInfo) -> ! {
    unsafe {
        asm!("li tp, 0", options(nomem));
    }

    let caps = unsafe { InitCapabilities::new() };
    let init_info = unsafe { &*init_info };

    {
        init_info
            .dev_pages
            .lo()
            .map(
                caps.pgtbl,
                VirtualConst::from_usize(0x8000_0000),
                PageTableFlags::RW,
            )
            .unwrap();

        // let captr_alloc_notif = root_captbl.create_object(()).unwrap();
        // let captr_alloc_pages: usize = (1 << init_info.captbl_size_log2) / (8 * 4.kib());
        // let mut free_slot_ctr = 1;
        // for n in 0..captr_alloc_pages {
        //     let pg: PageCaptr<BasePage> = root_captbl.create_object(()).unwrap();
        //     pg.map(
        //         caps.pgtbl,
        //         VirtualConst::from_usize(0xE000_0000 + n * 4.kib()),
        //         PageTableFlags::RW,
        //     )
        //     .unwrap();
        //     free_slot_ctr += 1;
        // }

        // // SAFETY: We have just initialized and mapped this memory.
        // let bitmap = unsafe {
        //     slice::from_raw_parts_mut(
        //         0xE000_0000 as *mut u64,
        //         (captr_alloc_pages * 4.kib()) / mem::size_of::<u64>(),
        //     )
        // };

        // let captr_alloc = CAPTR_ALLOC.get_or_init(|| {
        //     CaptrAllocator::new(bitmap, 1 << init_info.captbl_size_log2, captr_alloc_notif)
        // });

        // for n in 0..init_info.free_slots.lo.add(free_slot_ctr).into_raw() {
        //     captr_alloc.set_used(n);
        // }

        // SAFETY: The provided base address is valid and can be mapped.
        unsafe {
            GLOBAL_ALLOC
                .get_or_init(|| LinkedListAlloc::new(Notification::create().unwrap(), caps.pgtbl))
                .init(0xF000_0000 as *mut u8);
        }
    }

    let job = Job::create(caps.job).unwrap();
    let procsvr = job.create_thread("procsvr").unwrap();

    let procsvr_stack_1: Page<MegaPage> = Page::create().unwrap();
    let procsvr_stack_2: Page<MegaPage> = Page::create().unwrap();
    let procsvr_endpoint = Endpoint::create().unwrap();
    let procsvr_ipc_buf: Page<BasePage> = Page::create().unwrap();
    let procsvr_pgtbl = PageTable::create().unwrap();

    procsvr_stack_1
        .map(
            procsvr_pgtbl,
            VirtualConst::from_usize(0x1000_0000),
            PageTableFlags::RW,
        )
        .unwrap();
    procsvr_stack_2
        .map(
            procsvr_pgtbl,
            VirtualConst::from_usize(0x1020_0000),
            PageTableFlags::RW,
        )
        .unwrap();

    let bootfs = lz4_flex::decompress_size_prepended(BOOTFS).unwrap();

    // SAFETY: We assume the BOOTFS to be correct, by our build
    // script. Of course, if it's not, we have bigger problems.
    let procsvr_data = unsafe {
        let hdr = *bootfs.as_ptr().cast::<BootFsHeader>();
        debug_assert_eq!(hdr.magic, 0x0B007F50);
        let entries = slice::from_raw_parts(
            bootfs
                .as_ptr()
                .add(mem::size_of::<BootFsHeader>())
                .cast::<BootFsEntry>(),
            hdr.n_entries as usize,
        );
        println!(
            "Loaded bootfs (compressed size {} bytes) with {} {}",
            BOOTFS.len(),
            entries.len(),
            if entries.len() != 1 {
                "entries"
            } else {
                "entry"
            }
        );
        for x in entries {
            let s = core::str::from_utf8_unchecked(slice::from_raw_parts(
                bootfs.as_ptr().add(x.name_offset as usize),
                x.name_length as usize,
            ));

            println!("- {}: {} bytes", s, x.length);
        }

        let procsvr_entry = entries
            .iter()
            .find(|x| {
                let s = core::str::from_utf8_unchecked(slice::from_raw_parts(
                    bootfs.as_ptr().add(x.name_offset as usize),
                    x.name_length as usize,
                ));

                s == "procsvr"
            })
            .unwrap();
        slice::from_raw_parts(
            bootfs.as_ptr().add(procsvr_entry.offset as usize),
            procsvr_entry.length as usize,
        )
    };

    let procsvr_n_pages = procsvr_data.len().next_multiple_of(4096) / 4096;

    let mut procsvr_pages = vec![];
    for i in 0..procsvr_n_pages {
        let pg = Page::<BasePage>::create().unwrap();
        procsvr_pages.push(pg);
        pg.map(
            procsvr_pgtbl,
            VirtualConst::from_usize(0x1040_0000 + 4096 * i),
            PageTableFlags::RWX,
        )
        .unwrap();

        pg.map(
            caps.pgtbl,
            VirtualConst::from_usize(0x6000_0000),
            PageTableFlags::RW,
        )
        .unwrap();
        unsafe {
            ptr::copy_nonoverlapping(
                procsvr_data[i * 4096..(i + 1) * 4096].as_ptr(),
                0x6000_0000 as *mut _,
                4096,
            )
        };
    }

    unsafe {
        procsvr.configure(procsvr_pgtbl, procsvr_ipc_buf).unwrap();
        procsvr
            .start(
                VirtualConst::from_usize(0x1040_0000),
                VirtualMut::from_usize(0x1040_0000),
                procsvr_endpoint.into_captr(),
                procsvr_n_pages as u64,
            )
            .unwrap();
    }

    let procsvr_endpoint = procsvr_endpoint
        .grant(CapRights::all(), NonZeroU64::new(0xb007d00d))
        .unwrap();

    procsvr_endpoint
        .send_with_regs(MessageHeader::new(), *job, [0; 4])
        .unwrap();

    procsvr_endpoint
        .send_with_regs(MessageHeader::new(), *procsvr, [0; 4])
        .unwrap();

    procsvr_endpoint
        .send_with_regs(MessageHeader::new(), *procsvr_stack_1, [0; 4])
        .unwrap();
    procsvr_endpoint
        .send_with_regs(MessageHeader::new(), *procsvr_stack_2, [0; 4])
        .unwrap();

    procsvr_endpoint
        .send_with_regs(MessageHeader::new(), *procsvr_ipc_buf, [0; 4])
        .unwrap();

    procsvr_endpoint
        .send_with_regs(MessageHeader::new(), *procsvr_pgtbl, [0; 4])
        .unwrap();

    procsvr_endpoint
        .send_with_regs(MessageHeader::new(), *caps.thread, [0; 4])
        .unwrap();
    procsvr_endpoint
        .send_with_regs(MessageHeader::new(), *caps.job, [0; 4])
        .unwrap();

    for page in procsvr_pages {
        procsvr_endpoint
            .send_with_regs(MessageHeader::new(), *page, [0; 4])
            .unwrap();
    }

    let captr = Notification::create().unwrap().into_captr();
    for i in 0..procsvr_endpoint.into_raw() {
        let captr = Captr::from_raw(i);
        let _ = AnyCap::from_captr(captr).delete();
    }
    for i in (procsvr_endpoint.into_raw() + 1)..=captr.into_raw() {
        let captr = Captr::from_raw(i);
        let _ = AnyCap::from_captr(captr).delete();
    }

    procsvr_endpoint
        .send_with_regs(
            MessageHeader::new().with_length(4).with_private(0xdeadbeef),
            Captr::null(),
            [0xDEADBEEF, 0xC0DED00D, 0xC0DEBEEF, 0xDEADD00D],
        )
        .unwrap();

    loop {
        unsafe {
            core::arch::asm!("nop");
        }
    }
}

struct DebugPrint;

impl fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        syscalls::debug::debug_print_string(s);
        Ok(())
    }
}

struct DebugPrintBackup;

impl fmt::Write for DebugPrintBackup {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        print_backup(s);
        Ok(())
    }
}

pub fn print(s: &str) {
    if s.len() >= 4096 {
        todo!();
    }
    let tp = {
        let x: u64;
        unsafe { core::arch::asm!("mv {}, tp", out(reg) x, options(nostack)) };
        x
    };

    let base = if tp == 1 { 0x6100_0000 } else { 0x6000_0000 };
    unsafe { ptr::copy_nonoverlapping(s.as_ptr(), base as *mut _, s.len()) };
    unsafe { ptr::write((base as *mut u8).add(s.len()), 0) };
    unsafe {
        ecall7(
            SyscallNumber::EndpointCall,
            60000,
            MessageHeader::new()
                .with_length(cmp::max(8 * 5, (s.len() + 1).div_ceil(8)))
                .into_raw(),
            0,
            0,
            0,
            0,
            0,
        )
        .unwrap()
    };
}

pub fn print_backup(s: &str) {
    let serial = 0x80000000 as *mut u8;
    for b in s.as_bytes() {
        // SAFETY: The invariants of the serial driver ensure this is valid.
        while (unsafe { ptr::read_volatile(serial.add(5)) } & (1 << 5)) == 0 {
            core::hint::spin_loop();
        }
        // SAFETY: See above.
        unsafe { ptr::write_volatile(serial, *b) }
    }
}
