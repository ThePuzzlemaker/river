//! Opinionated userspace utilities that don't quite belong in `rille`.
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
#![feature(doc_cfg, doc_auto_cfg, allocator_api, slice_ptr_get)]

pub mod malloc;
pub mod sync;
