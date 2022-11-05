use core::{hint, mem, ptr};

use bitflags::bitflags;

use crate::{
    hart_local::LOCAL_HART,
    spin::{SpinMutex, SpinMutexGuard},
};

pub struct Ns16650 {
    inner: SpinMutex<Ns16650Inner>,
}

pub struct Ns16650Inner {
    serial_base: *mut u8,
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
const DIVISOR_LATCH_LSB: usize = 0b000;
const DIVISOR_LATCH_MSB: usize = 0b001;

impl Ns16650 {
    // TODO: make this better
    pub fn lock(&self) -> SpinMutexGuard<'_, Ns16650Inner> {
        self.inner.lock()
    }
}

// TODO: make this not Ns16650Inner
impl Ns16650Inner {
    /// # Safety
    ///
    /// This function MUST be called ONLY ONCE.
    pub unsafe fn init(&mut self, serial_base: *mut u8) {
        debug_assert!(
            !self.init,
            "Ns16650::init: tried to initialize an already initialized UART device"
        );
        self.serial_base = serial_base;
        self.init = true;

        // Disable interrupts
        self.write_ier(IERFlags::empty().bits);

        // Baud rate = 38.4K, msb=0 lsb=3
        self.set_clock_divisor(0x00, 0x03);

        // Reset and enable FIFOs.
        self.write_fcr(FCRFlags::FIFO_ENABLE.bits | FCRFlags::RESET_ALL_FIFOS.bits);

        // Enable IRQs. (Don't enable RX yet, since I haven't implemented it :P)
        self.write_ier(IERFlags::TX_IRQ_ENABLE.bits);
    }

    unsafe fn set_clock_divisor(&mut self, msb: u8, lsb: u8) {
        let lcr = self.read_lcr();
        self.write_lcr(lcr | LCRFlags::DIVISOR_LATCH_ENABLE.bits);

        self.serial_base.add(DIVISOR_LATCH_LSB).write_volatile(lsb);
        self.serial_base.add(DIVISOR_LATCH_MSB).write_volatile(msb);

        self.write_lcr(lcr);
    }

    unsafe fn read_lcr(&self) -> u8 {
        self.serial_base.add(LCR).read_volatile()
    }

    unsafe fn write_lcr(&mut self, data: u8) {
        self.serial_base.add(LCR).write_volatile(data);
    }

    unsafe fn write_ier(&mut self, data: u8) {
        self.serial_base.add(IER).write_volatile(data);
    }

    unsafe fn write_fcr(&mut self, data: u8) {
        self.serial_base.add(FCR).write_volatile(data);
    }

    unsafe fn read_lsr(&mut self) -> u8 {
        self.serial_base.add(LSR).read_volatile()
    }

    unsafe fn write_thr(&mut self, data: u8) {
        self.serial_base.add(THR).write_volatile(data);
    }

    unsafe fn putc_sync(&mut self, char: u8) {
        // this is probably not *strictly* necessary, since we hold a spinlock
        // anyway, but can't be too safe :P
        // may remove in the future based on perf considerations
        LOCAL_HART.with(|ctx| ctx.borrow_mut().push_off());

        // Spin until LSR gives the okay to put a bytes into THR
        while self.read_lsr() & LSRFlags::TX_HOLDING_IDLE.bits == 0 {
            hint::spin_loop();
        }
        self.write_thr(char);

        LOCAL_HART.with(|ctx| ctx.borrow_mut().pop_off());
    }
}

unsafe impl Sync for Ns16650 {}
unsafe impl Send for Ns16650 {}

pub static UART: Ns16650 = Ns16650 {
    inner: SpinMutex::new(Ns16650Inner {
        serial_base: ptr::null_mut(),
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
        const TX_HOLDING_IDLE = 1 << 5;
    }
}

pub fn handle_interrupt() {
    // for now, we do nothing, since we don't have a user mode and this interrupt is useful only for user mode :P
}
