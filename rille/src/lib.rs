//! # River Interface Library (`rille`)
//!
//! `rille`, also known as the River Interface Library, is the
//! counterpart to [`std::os`][1] for River. While not intending to be
//! a replacement for that module, it is intended to create a common
//! library of interface types, constants, helper functions, and other
//! useful constructs for use both in userspace code and the kernel.
//!
//! [1]: https://doc.rust-lang.org/stable/std/os/index.html

#![no_std]

pub mod capability;
