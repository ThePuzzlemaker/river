//! Utilities for getting the address of linked symbols.
//!
//! This probably won't be too useful to userspace, but it's nice to
//! have here.

/// An opaque reference to the address of some linker symbol.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Symbol(u8);

impl Symbol {
    /// Get a const pointer to this linker symbol.
    #[inline(always)]
    pub fn as_ptr(&self) -> *const u8 {
        (self as *const Symbol).cast::<u8>()
    }

    /// Get a mutable pointer to this linker symbol.
    #[inline(always)]
    pub fn as_ptr_mut(&self) -> *mut u8 {
        self as *const Symbol as *mut u8
    }

    /// Convert this linker symbol's pointer to a [`usize`].
    #[inline(always)]
    pub fn into_usize(&self) -> usize {
        self.as_ptr() as usize
    }
}

/// This macro, in combination with an `extern "C"` block, allows the
/// resolution of linker symbols with [`Symbol`].
///
/// # Example
///
/// Presume the linker symbols `__some_data_start` and
/// `__some_data_end` are defined.
///
/// ```ignore
/// use rille::symbol::Symbol;
/// use rille::impl_symbols;
///
/// extern "C" {
///     static __some_data_start: Symbol;
///     static __some_data_end: Symbol;
/// }
///
/// impl_symbols![
///     /// You can also put doc comments here.
///     some_data_start; __some_data_start,
///     /// This is the end of some data.
///     some_data_end; __some_data_end,
/// ];
/// ```
///
/// This will produce the following functions:
///
/// ```ignore
/// pub fn some_data_start() -> &'static Symbol { /* ... */ }
/// pub fn some_data_end() -> &'static Symbol { /* ... */ }
/// ```
///
/// These can then be used to get pointers and addresses of this
/// symbol:
///
/// ```ignore
/// let some_data_size = some_data_end().into_usize() - some_data_start().into_usize()
/// ```
#[macro_export]
macro_rules! impl_symbols {
    ($($(#[$meta:meta])* $name:ident; $sym:ident),+$(,)?) => {
        $($(#[$meta])*
	  #[allow(dead_code)]
	  #[inline]
	  pub fn $name() -> &'static $crate::symbol::Symbol {
            // SAFETY: We are just getting a pointer to the data, not
            // necessarily reading it.
            unsafe { &$sym }
        })+
    }
}

#[cfg(any(doc, feature = "kernel"))]
mod kernel_symbols {
    use super::Symbol;

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
        static __tbss_start: Symbol;
        static __tbss_end: Symbol;
        static TRAMPOLINE_START: Symbol;
        static trampoline: Symbol;
        static ret_user: Symbol;
        static user_code_woo: Symbol;
    }

    #[rustfmt::skip]
    impl_symbols![
        /// Start of all kernel data
        kernel_start; KERNEL_START,
        /// End of all kernel data
        kernel_end; KERNEL_END,

        /// Start of `.bss`
        bss_start; __bss_start,
        /// End of `.bss`
        bss_end; __bss_end,

        /// Start of `.data`
        data_start; __data_start,
        /// End of `.data`
        data_end; __data_end,

        /// Start (bottom) of `.tmp_stack`
        tmp_stack_bottom; __tmp_stack_bottom,
        /// End (top) of `.tmp_stack`
        tmp_stack_top; __tmp_stack_top,

        /// Start of `.text`
        text_start; __text_start,
        /// End of `.text`
        text_end; __text_end,

        /// Start of hart-local data `.tdata`
        tdata_start; __tdata_start,
        /// End of hart-local data `.tdata`
        tdata_end; __tdata_end,

	/// Start of hart-local data `.tbss`
	tbss_start; __tbss_start,
	/// End of hart-local data `.tbss`
	tbss_end; __tbss_end,

        /// Trampoline start
        trampoline_start; TRAMPOLINE_START,

        /// `trampoline` assembly function
        fn_trampoline; trampoline,

        /// `ret_user` assembly function
        fn_ret_user; ret_user,

        /// `user_code_woo` assembly function
        fn_user_code_woo; user_code_woo,
    ];
}

#[cfg(any(doc, feature = "kernel"))]
pub use kernel_symbols::*;
