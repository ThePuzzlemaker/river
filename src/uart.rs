use core::{
    fmt, hint, ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use bitflags::bitflags;

use crate::{
    paging,
    spin::{SpinMutex, SpinMutexGuard},
};

#[macro_export]
macro_rules! println {
    ($fmt:literal$(, $($tt:tt)*)?) => {{
        let mut uart = $crate::uart::UART.lock();
        ::core::fmt::write(&mut *uart, ::core::format_args!($fmt$(, $($tt)*)?)).expect("UART write");
        uart.print_str_sync("\n");
    }}
}

pub struct Ns16650 {
    inner: SpinMutex<Ns16650Inner>,
}

pub struct Ns16650Inner {
    serial_base: *mut u8,
    serial_base_backup: *mut u8,
    init: bool,
    tx_buffer: [u8; 1024],
    tx_buffer_r: usize,
    tx_buffer_w: usize,
}

const LCR: usize = 0b011;
const IER: usize = 0b001;
const FCR: usize = 0b010;
const LSR: usize = 0b101;
const THR: usize = 0b000;
const RHR: usize = 0b000;
const DIVISOR_LATCH_LSB: usize = 0b000;
const DIVISOR_LATCH_MSB: usize = 0b001;

pub static SERIAL_BACKUP: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());
pub static SERIAL: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());

impl Ns16650 {
    // TODO: make this better
    #[inline]
    pub fn lock(&self) -> SpinMutexGuard<'_, Ns16650Inner> {
        self.inner.lock()
    }
}

// TODO: make this not Ns16650Inner
impl Ns16650Inner {
    #[inline]
    pub fn is_initialized(&self) -> bool {
        self.init
    }

    #[inline]
    fn serial_base(&self) -> *mut u8 {
        if paging::enabled() {
            self.serial_base
        } else {
            self.serial_base_backup
        }
    }

    /// # Safety
    ///
    /// This function MUST be called ONLY ONCE.
    pub unsafe fn init(&mut self, serial_base: *mut u8) {
        debug_assert!(
            !self.init,
            "Ns16650::init: tried to initialize an already initialized UART device"
        );
        self.serial_base = serial_base;
        SERIAL.store(serial_base, Ordering::Relaxed);
        self.serial_base_backup = self.serial_base;
        SERIAL_BACKUP.store(self.serial_base, Ordering::Relaxed);
        self.init = true;

        // Disable interrupts
        unsafe { self.write_ier(IERFlags::empty().bits) };

        // Baud rate = 38.4K, msb=0 lsb=3
        unsafe { self.set_clock_divisor(0x00, 0x03) };

        // Reset and enable FIFOs.
        unsafe { self.write_fcr(FCRFlags::FIFO_ENABLE.bits | FCRFlags::RESET_ALL_FIFOS.bits) };

        // Enable IRQs. (Don't enable RX yet, since I haven't implemented it :P)
        unsafe { self.write_ier(IERFlags::TX_IRQ_ENABLE.bits) };
    }

    pub unsafe fn update_serial_base(&mut self, serial_base: *mut u8) {
        self.serial_base = serial_base;
        SERIAL.store(serial_base, Ordering::Relaxed);
    }

    unsafe fn set_clock_divisor(&mut self, msb: u8, lsb: u8) {
        let lcr = unsafe { self.read_lcr() };
        unsafe { self.write_lcr(lcr | LCRFlags::DIVISOR_LATCH_ENABLE.bits) };

        unsafe {
            self.serial_base()
                .add(DIVISOR_LATCH_LSB)
                .write_volatile(lsb)
        }
        unsafe {
            self.serial_base()
                .add(DIVISOR_LATCH_MSB)
                .write_volatile(msb)
        }

        unsafe { self.write_lcr(lcr) }
    }

    unsafe fn read_lcr(&self) -> u8 {
        unsafe { self.serial_base().add(LCR).read_volatile() }
    }

    unsafe fn write_lcr(&mut self, data: u8) {
        unsafe { self.serial_base().add(LCR).write_volatile(data) }
    }

    unsafe fn write_ier(&mut self, data: u8) {
        unsafe { self.serial_base().add(IER).write_volatile(data) }
    }

    unsafe fn write_fcr(&mut self, data: u8) {
        unsafe { self.serial_base().add(FCR).write_volatile(data) }
    }

    unsafe fn read_lsr(&mut self) -> u8 {
        unsafe { self.serial_base().add(LSR).read_volatile() }
    }

    unsafe fn write_thr(&mut self, data: u8) {
        unsafe { self.serial_base().add(THR).write_volatile(data) }
    }

    pub fn putc_sync(&mut self, char: u8) {
        // Spin until LSR gives the okay to put a bytes into THR
        while unsafe { self.read_lsr() } & LSRFlags::TX_HOLDING_IDLE.bits == 0 {
            hint::spin_loop();
        }
        unsafe { self.write_thr(char) }
    }

    unsafe fn read_rhr(&mut self) -> u8 {
        unsafe { self.serial_base().add(RHR).read_volatile() }
    }

    pub fn getc_sync(&mut self) -> Option<u8> {
        if unsafe { self.read_lsr() } & LSRFlags::RX_DATA_READY.bits != 0 {
            Some(unsafe { self.read_rhr() })
        } else {
            None
        }
    }

    pub fn print_str_sync(&mut self, s: &str) {
        for b in s.as_bytes() {
            self.putc_sync(*b)
        }
    }
}

unsafe impl Sync for Ns16650 {}
unsafe impl Send for Ns16650 {}

pub static UART: Ns16650 = Ns16650 {
    inner: SpinMutex::new(Ns16650Inner {
        serial_base: ptr::null_mut(),
        serial_base_backup: ptr::null_mut(),
        init: false,
        tx_buffer: [0; 1024],
        tx_buffer_r: 0,
        tx_buffer_w: 0,
    }),
};

bitflags! {
    struct LCRFlags: u8 {
        const DIVISOR_LATCH_ENABLE = 1 << 7;
    }
    struct IERFlags: u8 {
        const RX_IRQ_ENABLE = 1 << 0;
        const TX_IRQ_ENABLE = 1 << 1;
    }
    struct FCRFlags: u8 {
        const FIFO_ENABLE = 1 << 0;
        const RX_FIFO_RESET = 1 << 1;
        const TX_FIFO_RESET = 1 << 2;
        const RESET_ALL_FIFOS = Self::RX_FIFO_RESET.bits | Self::TX_FIFO_RESET.bits;
    }
    struct LSRFlags: u8 {
        const RX_DATA_READY = 1 << 0;
        const TX_HOLDING_IDLE = 1 << 5;
    }
}

pub fn handle_interrupt() {
    // for now, we do nothing, since we don't have a user mode and this interrupt is useful only for user mode :P
}

impl fmt::Write for Ns16650Inner {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        debug_assert!(
            self.is_initialized(),
            "Ns16650Inner::write_str: UART was not initialized (this should not happen?)"
        );
        self.print_str_sync(s);
        Ok(())
    }
}

pub fn print_backup(s: &str) {
    let serial = if paging::enabled() {
        SERIAL.load(Ordering::Relaxed)
    } else {
        SERIAL_BACKUP.load(Ordering::Relaxed)
    };
    for b in s.as_bytes() {
        while (unsafe { ptr::read_volatile(serial.add(5)) } & (1 << 5)) == 0 {
            hint::spin_loop();
        }
        unsafe { ptr::write_volatile(serial, *b) }
    }
}

#[macro_export]
macro_rules! println_backup {
    ($fmt:literal$(, $($tt:tt)*)?) => {{
        ::core::fmt::write(&mut $crate::uart::FmtBackup, ::core::format_args!($fmt$(, $($tt)*)?)).expect("i/o");
        $crate::uart::print_backup("\n");
    }}
}

pub struct FmtBackup;
impl fmt::Write for FmtBackup {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        print_backup(s);
        Ok(())
    }
}
