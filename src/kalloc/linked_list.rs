//! A linked list allocator.
//!
//! This allocator is vaguely based off an algorithm I wrote for
//! [knight.wat](https://github.com/ThePuzzlemaker/knight.wat). That algorithm
//! was based on a vague understanding of a video I found on allocators, and a
//! bit of elbow grease and luck led to a working solution (with one janky edge
//! case--growing the heap--which here I can likely solve more easily).
//!
//! TODO: I'll have to find a link to that video and put it here and in
//! knight.wat's source code
//!
//! In summary:
//! - Freed nodes contain an explicit doubly linked free list.
//! - All nodes contain a size tag, with some padding potentially before the
//!   data. There is never padding added after data, excluding when blocks are
//!   not split. This size tag has the highest bit set when the node is free.
//!   This eliminates the need for a separate byte (or integer, as would likely
//!   be for alignment purposes) to determine freed nodes.
//! - At a fixed offset (`1*mem::size_of::<usize>()`) before the data
//!   (potentially inside of padding) in occupied nodes, the number of bytes
//!   added for alignment purposes (potentially including this padding size tag)
//!   is used.
//! - One notable difference: this algorithm will **not** use boundary tags.
//!   Instead, it will actually (properly) use the doubly-linked list that was
//!   present in the old algorithm for the purposes of back-coalescing. (I had
//!   once tried to port this to Rust with boundary tags, but alignment and
//!   safety requirements of Rust's allocation traits made it very difficult and
//!   annoying).
//! - Nodes are chosen by first-fit, as opposed to best-fit in the original
//!   algorithm.
//! - Freed nodes are inserted into the list in address order. (I might do
//!   something with seglists or some other type of ordering in the future, I
//!   don't know).
//! - Freeing nodes will coalesce (specifically, first coalescing forward, as
//!   it's the simplest to implement--less pointer indirection, then coalescing
//!   backward using backlinks--this doesn't really matter though).
//! - The root node of the heap contains a pointer to the beginning of the free
//!   list, and the beginning of the unmanaged region of the heap, as well as
//!   some other metadata.
//! - If there is wasted space at the end of a block, and this size is above a
//!   certain (tunable) threshold, the block will be split into two separate
//!   blocks, with the latter block being inserted into the free list and the
//!   former block being spliced out and returned.

use core::{
    alloc::{AllocError, Allocator, Layout},
    mem::{self, MaybeUninit},
    ptr::{self, addr_of_mut, NonNull},
};

use alloc::slice;

use crate::{
    addr::{Identity, VirtualMut},
    println_hacky,
    spin::SpinMutex,
    units,
};

// It's unlikely the kernel itself is 64GiB in size, so we use this space.
pub const KHEAP_VMEM_OFFSET: usize = 0xFFFFFFE000000000;
pub const KHEAP_VMEM_SIZE: usize = 64 * units::GIB;

#[derive(Debug)]
pub struct LinkedListAlloc {
    pub inner: SpinMutex<LinkedListAllocInner>,
}

#[derive(Debug)]
pub struct LinkedListAllocInner {
    pub init: bool,
    pub mapped_size: usize,
    pub unmanaged_ptr: *mut u8,
    pub free_list: *mut FreeNode,
}

#[derive(Debug)]
#[repr(C)]
pub struct FreeNode {
    size_tag: usize,
    next: *mut FreeNode,
    prev: *mut FreeNode,
}

fn calculate_needed_size(node_base: *mut u8, layout: Layout) -> (usize, usize) {
    let n_bytes_padding = node_base.align_offset(layout.align());
    // N.B. We don't need to round up the `Layout`'s size since we make
    // no assumptions about the alignment of blocks following this one
    // (as those allocations will just insert their own padding as
    // necessary, anyway!)
    let needed_size = layout.size() + n_bytes_padding;
    (needed_size, n_bytes_padding)
}

enum FoundNode {
    New {
        ptr: *mut FreeNode,
        needed_size: usize,
        n_bytes_padding: usize,
        new_unmanaged_ptr: *mut u8,
    },
    Old {
        ptr: *mut FreeNode,
        node_size: usize,
        needed_size: usize,
        n_bytes_padding: usize,
    },
}

impl LinkedListAllocInner {
    unsafe fn find_first_fit(&self, layout: Layout) -> FoundNode {
        let mut current_node = self.free_list;

        loop {
            println_hacky!("find_first_fit loop: next={:#p}", current_node);

            if current_node.is_null() {
                // Our first-fit block wasn't found, i.e. there wasn't a block
                // large enough. This means we need to make a new block, which
                // potentially means expanding the memory.
                let new_node = self.unmanaged_ptr;
                println_hacky!("find_first_fit: new_node/unmanaged_ptr={:#p}", new_node);

                let node_base = new_node.add(mem::size_of::<usize>());
                println_hacky!("find_first_fit: new_node node_base={:#p}", node_base);
                let (needed_size, n_bytes_padding) = calculate_needed_size(node_base, layout);
                println_hacky!(
                    "find_first_fit: needed_size={:#x} n_bytes_padding={}",
                    needed_size,
                    n_bytes_padding
                );
                if KHEAP_VMEM_OFFSET + self.mapped_size - (node_base as usize) < needed_size {
                    todo!("grow kheap");
                }
                let new_unmanaged_ptr = node_base.add(needed_size);
                println_hacky!("find_first_fit: new_unmanaged_ptr={:#p}", new_unmanaged_ptr);
                return FoundNode::New {
                    ptr: new_node.cast(),
                    needed_size,
                    n_bytes_padding,
                    new_unmanaged_ptr,
                };
            }

            // The MSB of the size is the "is free" bit, so we need to take that
            // out to make sure our reference size is right.
            let size_tag = unsafe { &*current_node }.size_tag;
            debug_assert_ne!(
                size_tag & (1 << 63),
                0,
                "free list node referred to occupied block"
            );
            // N.B. doesn't include this size tag
            let node_size = size_tag & !(1 << 63);
            let node_base = current_node.cast::<u8>().add(mem::size_of::<usize>());
            let (needed_size, n_bytes_padding) = calculate_needed_size(node_base, layout);
            // This node is not for us. Move on!
            if needed_size < node_size {
                current_node = unsafe { &*current_node }.next;
                continue;
            }
            // This node is good for us. Return size and calculated padding.
            return FoundNode::Old {
                ptr: current_node,
                node_size,
                needed_size,
                n_bytes_padding,
            };
        }
    }
}

unsafe impl Allocator for LinkedListAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        println_hacky!("linked list alloc: allocate: layout={:?}", layout);
        let mut alloc = self.inner.lock();
        let node = unsafe { alloc.find_first_fit(layout) };

        let ptr = match node {
            FoundNode::New {
                ptr,
                needed_size,
                mut n_bytes_padding,
                new_unmanaged_ptr,
            } => unsafe {
                ptr.cast::<usize>().write(needed_size);
                if n_bytes_padding == 0 {
                    // This is kind of inefficient but I think it's the best we
                    // can do.

                    // TODO: look into this

                    // N.B. if layout.align() == mem::size_of::<usize>(), having
                    // 0 bytes of padding is still well-aligned, even with the
                    // padding-size tag
                    // TODO: is this right?
                    n_bytes_padding = layout.align().saturating_sub(mem::size_of::<usize>());
                }
                ptr.cast::<u8>()
                    .add(n_bytes_padding)
                    .cast::<usize>()
                    .write(n_bytes_padding);
                alloc.unmanaged_ptr = new_unmanaged_ptr;
                ptr.cast::<u8>()
                    .add(mem::size_of::<usize>())
                    .add(n_bytes_padding)
            },
            FoundNode::Old {
                ptr,
                needed_size,
                n_bytes_padding,
                ..
            } => unsafe {
                let mut node = &mut *ptr;
                node.size_tag = needed_size;
                // N.B. null prev == node is at head of free-list
                if node.prev.is_null() {
                    alloc.free_list = node.next;
                } else {
                    (*node.prev).next = node.next;
                }
                // Don't need to do anything here if this is at the tail of the
                // free-list.
                if !node.next.is_null() {
                    (*node.next).prev = node.prev;
                }
                ptr.cast::<u8>()
                    .add(mem::size_of::<usize>())
                    .add(n_bytes_padding)
            },
        };

        debug_assert_eq!(
            ptr as usize % layout.align(),
            0,
            "kalloc::linked_list: allocator allocated unaligned block"
        );

        // TODO: split blocks
        Ok(unsafe { NonNull::new_unchecked(slice::from_raw_parts_mut(ptr, layout.size())) })
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: This pointer is safe to use as it refers to data within the
        // same linked list allocator node (specifically, the node's metadata).
        let padding_size_ptr = ptr.as_ptr().cast::<usize>().wrapping_sub(1);
        let padding_size = unsafe { *padding_size_ptr };
        // SAFETY: Same as above.
        let size_tag_ptr = ptr.as_ptr().wrapping_sub(padding_size).cast::<usize>();
        todo!();
    }
}

pub const BLOCK_SPLIT_THRESHOLD: usize = 64 /* bytes */;
