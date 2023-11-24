//! # River Interface Library (`rille`)
//!
//! `rille`, also known as the River Interface Library, is the
//! counterpart to [`std::os`][1] for River. While not intending to be
//! a replacement for that module, it is intended to create a common
//! library of interface types, constants, helper functions, and other
//! useful constructs for use both in userspace code and the kernel.
//!
//! ## A note on features
//!
//! As an interface library, `rille` is used in the kernel of
//! river. For this reason, there is a feature known as `kernel` that
//! will enable some extra features of the library for interface
//! consistency purposes. This should not be activated in any crate
//! but the kernel, as it assumes the crate being built is the kernel,
//! and may cause some linker issues.
// TODO: move some of the kernel-specific stuff *back* to the kernel.
//!
//! [1]: https://doc.rust-lang.org/stable/std/os/index.html

#![no_std]
#![warn(
    clippy::undocumented_unsafe_blocks,
    clippy::pedantic,
    missing_docs,
//  clippy::todo
)]
#![allow(
    clippy::inline_always,
    clippy::must_use_candidate,
    clippy::cast_possible_truncation,
    clippy::module_name_repetitions,
    clippy::cast_ptr_alignment,
    clippy::cast_lossless,
    clippy::too_many_lines,
    clippy::fn_to_numeric_cast
)]
#![feature(doc_cfg, doc_auto_cfg)]

pub mod addr;
pub mod capability;
pub mod elf;
pub mod init;
pub mod io_traits;
pub mod symbol;
pub mod syscalls;
pub mod units;
