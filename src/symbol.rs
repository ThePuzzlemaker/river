#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Symbol(u8);

impl Symbol {
    #[inline(always)]
    pub fn as_ptr(&self) -> *const u8 {
        (self as *const Symbol).cast::<u8>()
    }

    #[inline(always)]
    pub fn as_ptr_mut(&self) -> *mut u8 {
        self as *const Symbol as *mut u8
    }

    #[inline(always)]
    pub fn into_usize(&self) -> usize {
        self.as_ptr() as usize
    }
}

macro_rules! impl_symbols {
    ($($name:ident; $sym:ident),+$(,)?) => {
        $(#[allow(dead_code)] pub fn $name() -> &'static Symbol {
            // SAFETY: We are just getting a pointer to the data, not
            // necessarily reading it.
            unsafe { &$sym }
        })+
    }
}

extern "C" {
    static KERNEL_START: Symbol;
    static KERNEL_END: Symbol;
    static __bss_start: Symbol;
    static __bss_end: Symbol;
    static __data_start: Symbol;
    static __data_end: Symbol;
    static __tmp_stack_bottom: Symbol;
    static __tmp_stack_top: Symbol;
    static __text_start: Symbol;
    static __text_end: Symbol;
    static __tdata_start: Symbol;
    static __tdata_end: Symbol;
}

impl_symbols![
    kernel_start; KERNEL_START,
    kernel_end; KERNEL_END,
    bss_start; __bss_start,
    bss_end; __bss_end,
    data_start; __data_start,
    data_end; __data_end,
    tmp_stack_bottom; __tmp_stack_bottom,
    tmp_stack_top; __tmp_stack_top,
    text_start; __text_start,
    text_end; __text_end,
    tdata_start; __tdata_start,
    tdata_end; __tdata_end
];
