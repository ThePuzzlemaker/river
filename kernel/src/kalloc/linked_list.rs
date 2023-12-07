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
//!
//! As a final note, here are some assorted notes about the internal
//! workings of this code:
//! - `wrapping_add` for pointers is used often, and is admittable as the
//!   placement of our heap in vmem means that it would take a few TiB of
//!   memory usage for that pointer to overwrap in a "nominal" (i.e. non-
//!   clobbered) case

use core::{
    alloc::{AllocError, Allocator, GlobalAlloc, Layout},
    cmp, fmt, intrinsics, mem,
    ptr::{self, addr_of_mut, NonNull},
    slice,
};

use rille::{
    addr::{Identity, Virtual, VirtualMut},
    units::StorageUnits,
};

use crate::{
    kalloc::phys::{self, PMAlloc},
    paging::{root_page_table, PageTableFlags},
    sync::SpinMutex,
};

#[derive(Debug)]
pub struct LinkedListAlloc {
    inner: SpinMutex<LinkedListAllocInner>,
}

// SAFETY: The `LinkedListAllocatorInner` inside is protected by a `SpinMutex`.
unsafe impl Send for LinkedListAlloc {}
// SAFETY: See above.
unsafe impl Sync for LinkedListAlloc {}

impl LinkedListAlloc {
    pub const fn new() -> Self {
        Self {
            inner: SpinMutex::new(LinkedListAllocInner {
                init: false,
                mapped_size: 0,
                base: VirtualMut::null(),
                unmanaged_ptr: ptr::null_mut(),
                free_list: ptr::null_mut(),
            }),
        }
    }

    // TODO: make sure there's a limiting address.

    /// Initialize the [`LinkedListAlloc`].
    ///
    /// # Safety
    ///
    /// The pointer provided must be page-aligned and must not be aliased.  
    pub unsafe fn init(&self, base: VirtualMut<u8, Identity>) {
        let mut alloc = self.inner.lock();
        alloc.base = base;
        alloc.unmanaged_ptr = base.into_ptr_mut();
        alloc.init = true;
    }
}

struct LinkedListAllocInner {
    init: bool,
    mapped_size: usize,
    base: VirtualMut<u8, Identity>,
    unmanaged_ptr: *mut u8,
    free_list: *mut FreeNode,
}

impl fmt::Debug for LinkedListAllocInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkedListAllocInner")
            .field("init", &self.init)
            .field("mapped_size", &self.mapped_size)
            .field("base", &self.base)
            .field("unmanaged_ptr", &self.unmanaged_ptr)
            .field("free_list", &FreeListDebugAdapter(self.free_list))
            .finish()
    }
}

struct FreeListDebugAdapter(*mut FreeNode);

impl fmt::Debug for FreeListDebugAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut l = f.debug_list();
        let mut current = self.0;
        loop {
            if current.is_null() {
                break;
            }
            // SAFETY: By invariants.
            let node = unsafe { &*current };
            l.entry(&(current, node));
            current = node.next;
        }
        l.finish()
    }
}

#[derive(Debug)]
#[repr(C, align(8))]
pub struct FreeNode {
    size_tag: usize,
    next: *mut FreeNode,
    prev: *mut FreeNode,
}

fn calculate_needed_size(node_base: *mut u8, layout: Layout) -> (usize, usize) {
    // Make sure the minimum alignment is at least 8. This is
    // *probably* covered by the n_bytes_padding check, but it
    // *really* can't hurt.
    //
    // SAFETY: The previous layout is valid, and the only possible
    // values for align are the existing value (valid) and 8 (valid).
    let layout =
        unsafe { Layout::from_size_align_unchecked(layout.size(), cmp::max(layout.align(), 8)) };
    let mut n_bytes_padding = node_base.align_offset(layout.align());
    // make sure this node is at least big enough to hold a freed node
    let min_layout_size = cmp::max(layout.size(), MIN_NODE_SIZE);
    // calculate the amount of padding added from the above calculation, so that
    // we don't need to add this padding twice for the bytes of padding
    let min_size_compensation = min_layout_size - layout.size();
    if n_bytes_padding < mem::size_of::<usize>() {
        // make sure that we have enough padding space to put the
        // number of bytes of padding we used at 1*usize before the
        // data ptr. but while doing this, ensure that we don't
        // accidentally unalign our desired data (hence the
        // `align_to`)
        //
        // TODO: is not incorporating the min layout size compensation
        // into this value wasteful, or even valid? probably, but i'll
        // need to check that another day when i have a bit more brain
        // power left
        n_bytes_padding = Layout::new::<usize>()
            .align_to(layout.align())
            .unwrap()
            .align();
    }
    // N.B. We don't need to round up the `Layout`'s size since we
    // make no assumptions about the alignment of blocks following
    // this one beyond being 8-aligned (as those allocations will just
    // insert their own padding as necessary, anyway!)
    //
    // N.B. we don't add the min_size_compensation to n_bytes_padding
    // as that can be put at the end of the node; and besides, the
    // minimum node must still be 8-aligned, which n_bytes_padding
    // assumes.
    let needed_size = layout.size() + cmp::max(n_bytes_padding, min_size_compensation);
    // However, we *do* need to ensure that needed_size is at least
    // 8-aligned, so that our next size tag isn't unaligned. This
    // padding is at the end so we don't compensate in n_bytes_padding.
    let needed_size = needed_size.next_multiple_of(8);
    (needed_size, n_bytes_padding)
}

enum FoundNode {
    New {
        ptr: *mut FreeNode,
        needed_size: usize,
        n_bytes_padding: usize,
        new_unmanaged_ptr: *mut u8,
        grow_heap: bool,
    },
    Old {
        ptr: *mut FreeNode,
        node_size: usize,
        needed_size: usize,
        n_bytes_padding: usize,
    },
}

impl LinkedListAllocInner {
    #[track_caller]
    unsafe fn find_first_fit(&self, layout: Layout) -> FoundNode {
        //        log::trace!("find_first_fit: {layout:?}");
        let mut current_node = self.free_list;

        loop {
            //          log::trace!("find_first_fit: current_node={:#?}", current_node);
            if current_node.is_null() {
                // Our first-fit block wasn't found, i.e. there wasn't a block
                // large enough. This means we need to make a new block, which
                // potentially means expanding the memory.
                let new_node = self.unmanaged_ptr;

                // points to the beginning of the padding (new_node + sizeof(size_tag))
                let node_base = new_node.wrapping_add(mem::size_of::<usize>());
                let (needed_size, n_bytes_padding) = calculate_needed_size(node_base, layout);
                let available_space =
                    (self.base.into_usize() + self.mapped_size).saturating_sub(node_base as usize);
                let grow_heap = available_space < needed_size;
                let new_unmanaged_ptr = node_base.wrapping_add(needed_size);
                // Make sure the new unmanaged pointer is well-aligned for the next node.
                // TODO: is this calculation necessary now that I fixed this in needed_size?
                let new_unmanaged_ptr = new_unmanaged_ptr
                    .wrapping_add(new_unmanaged_ptr.align_offset(mem::align_of::<usize>()));
                return FoundNode::New {
                    ptr: new_node.cast(),
                    needed_size,
                    n_bytes_padding,
                    new_unmanaged_ptr,
                    grow_heap,
                };
            }

            // log::trace!("find_first_fit: current_node={:?}", unsafe {
            //     &*current_node
            // });

            // The MSB of the size is the "is free" bit, so we need to take that
            // out to make sure our reference size is right.
            //
            // SAFETY: We have ensured that the current node is not
            // null, and if this got clobbered we have bigger problems.
            let size_tag = unsafe { &*current_node }.size_tag;
            debug_assert_ne!(
                size_tag & (1 << 63),
                0,
                "free list node referred to occupied block: {current_node:#?}, size tag={size_tag:#x}, self={self:#?}",
            );
            // N.B. doesn't include this free tag
            let node_size = size_tag & !(1 << 63);

            // points to the beginning of the padding (current_node + sizeof(usize))
            let node_base = current_node
                .cast::<u8>()
                .wrapping_add(mem::size_of::<usize>());
            let (needed_size, n_bytes_padding) = calculate_needed_size(node_base, layout);
            // This node is not for us. Move on!
            if node_size < needed_size {
                // SAFETY: We have ensured that this node is not null.
                current_node = unsafe { &*current_node }.next;
                continue;
            }
            // This node is good for us. Return size and calculated
            // padding.
            return FoundNode::Old {
                ptr: current_node,
                node_size,
                needed_size,
                n_bytes_padding,
            };
        }
    }

    #[track_caller]
    fn free_list_valid(&self) -> bool {
        let mut prev = ptr::null_mut();
        let mut cur = self.free_list;
        loop {
            if cur.is_null() {
                break true;
            }
            // SAFETY: By invariants.
            let node = unsafe { &*cur };
            if (node.prev != prev) || (node.size_tag & (1 << 63)) == 0 {
                break false;
            }
            prev = cur;
            cur = node.next;
        }
    }
}

// FreeNode::next and FreeNode::prev (the size tag is universal, and
// not counted in node sizes)
const MIN_NODE_SIZE: usize = 2 * mem::size_of::<usize>();

// TODO: Implement grow and shrink logic on Allocator and realloc on
// GlobalAlloc to coalesce/split blocks when possible instead of
// dealloc/realloc-ing, since we can save some time and possibly space
// there on the short path, especially when new_size < old_size

// UNWIND SAFETY: our kernel does not unwind. it is simply not
// implemented.
unsafe impl GlobalAlloc for LinkedListAlloc {
    #[track_caller]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.allocate(layout)
            .map(NonNull::as_mut_ptr)
            .ok()
            .unwrap_or_else(ptr::null_mut)
    }

    #[track_caller]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: Our caller guarantees this is safe.
        unsafe { self.deallocate(NonNull::new_unchecked(ptr), layout) }
    }
}

// SAFETY: From the points on the documentation of [`Allocator`]:
// 1. Currently allocated memory blocks are valid until
//    deallocated while the allocator is alive.
// 2. LinkedListAlloc is !Copy & !Clone, so this point
//    does not apply.
// 3. Any pointer to a currently allocated block can be
//    passed to any other function on the allocator.
unsafe impl Allocator for LinkedListAlloc {
    #[track_caller]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let mut alloc = self.inner.lock();
        debug_assert!(
            alloc.init,
            "LinkedListAlloc::allocate: kalloc not initialized"
        );
        //        log::trace!("aaa");
        //log::trace!("alloc before: {alloc:#?}");
        debug_assert!(
            alloc.free_list_valid(),
            "LinkedListAlloc::allocate: free list was invalid before allocation {alloc:#?}"
        );
        // SAFETY: Our allocator is initialized.
        let node = unsafe { alloc.find_first_fit(layout) };

        let ptr = match node {
            FoundNode::New {
                ptr,
                needed_size,
                n_bytes_padding,
                new_unmanaged_ptr,
                grow_heap,
            } => {
                if intrinsics::unlikely(grow_heap) {
                    let order = phys::what_order(needed_size);
                    let page = {
                        let mut pma = PMAlloc::get();
                        pma.allocate(order).ok_or(AllocError)?
                    };
                    {
                        let mut pgtbl = root_page_table().lock();
                        // TODO: this would probably be a lot better with huge pages
                        for i in 0..(1 << order) {
                            pgtbl.map(
                                page.into_identity().into_const().add(i * 4.kib()),
                                Virtual::from_usize(
                                    alloc.base.into_usize() + alloc.mapped_size + i * 4.kib(),
                                ),
                                PageTableFlags::VAD | PageTableFlags::READ | PageTableFlags::WRITE,
                            );
                        }
                        alloc.mapped_size += 4.kib() * (1 << order);
                    }
                }

                // SAFETY: This pointer, by the invariants of
                // find_first_fit, is valid and aligned.
                unsafe {
                    ptr.cast::<usize>().write(needed_size);
                };

                debug_assert_ne!(n_bytes_padding, 0);
                // SAFETY: The base pointer is valid. Adding
                // n_bytes_padding to this pointer is also valid. Thus
                // the remaining pointer is valid to write to.
                //
                // Alignment is more tricky to argue:
                // - Note that the minimum alignment for any block is
                // 8 bytes.
                // - I did a little trickery with this
                // addition. Perhaps I should make it more clear, but
                // for now I'll document it: `ptr` points to the
                // begining of the node, **including** the size
                // tag. By adding `n_bytes_padding` but not
                // compensating for another `usize`, we end up
                // `1*usize` before the data (which must be at least
                // 8-aligned), as by compensating for the usize, we'd
                // end up directly at the data pointer.
                // - Thus, as it is 8 bytes (sizeof(usize)) before the
                // data pointer (8-aligned at minimum), this pointer
                // is properly aligned.
                // [REF 1]
                unsafe {
                    ptr.cast::<u8>()
                        .add(n_bytes_padding)
                        .cast::<usize>()
                        .write(n_bytes_padding);
                };
                alloc.unmanaged_ptr = new_unmanaged_ptr;
                // SAFETY: The base pointer is valid, and we properly
                // offset the pointer to the data.
                unsafe {
                    ptr.cast::<u8>()
                        .add(n_bytes_padding)
                        .add(mem::size_of::<usize>())
                }
            }
            FoundNode::Old {
                ptr,
                needed_size,
                n_bytes_padding,
                node_size,
            } => {
                // Check if our size excess is more than our threshold.
                if
                // node_size - needed_size >= BLOCK_SPLIT_THRESHOLD
                false {
                    // SAFETY: This pointer is valid as needed_size
                    // accounts for the data size *and* the padding
                    // size, and is also 8-aligned. Also, we account
                    // for the size tag as well; thus, this pointer
                    // points to the beginning of the properly-aligned
                    // excess block
                    let next = unsafe {
                        ptr.cast::<u8>()
                            .add(needed_size)
                            .add(mem::size_of::<usize>())
                            .cast::<FreeNode>()
                    };

                    let next_size = node_size - needed_size;

                    // SAFETY: We have exclusive access to all free
                    // nodes, by our invariants. Additionally, this
                    // ptr is valid and aligned.
                    let node = unsafe { &mut *ptr };

                    node.size_tag = needed_size;

                    // SAFETY: Ibid.
                    unsafe {
                        addr_of_mut!((*next).size_tag).write(next_size | (1 << 63));
                        addr_of_mut!((*next).prev).write(node.prev);
                        addr_of_mut!((*next).next).write(node.next);
                    }

                    // N.B. null prev == node is at head of free-list
                    if node.prev.is_null() {
                        alloc.free_list = next;
                    } else {
                        // SAFETY: Exclusive access by invariants &
                        // null check above.
                        unsafe { (*node.prev).next = next };
                    }

                    if !node.next.is_null() {
                        // SAFETY: Exclusive access by invariants &
                        // null check above.
                        unsafe { (*node.next).prev = next };
                    }

                    debug_assert_ne!(n_bytes_padding, 0);
                    // SAFETY: Again with the trickery, past me! See
                    // the long comment marked with `[REF 1]`
                    // above. The same argument applies.
                    unsafe {
                        ptr.cast::<u8>()
                            .add(n_bytes_padding)
                            .cast::<usize>()
                            .write(n_bytes_padding);
                    }

                    // SAFETY: The base ptr is valid and we properly
                    // offset to the data.
                    unsafe {
                        ptr.cast::<u8>()
                            .add(n_bytes_padding)
                            .add(mem::size_of::<usize>())
                    }
                } else {
                    // SAFETY: We have exclusive access to all free
                    // nodes by our invariants. Additionally, this
                    // pointer is valid and aligned.
                    let node = unsafe { &mut *ptr };
                    node.size_tag = node_size;

                    //log::trace!("{:?}", alloc);
                    // N.B. null prev == node is at head of free-list
                    if node.prev.is_null() {
                        alloc.free_list = node.next;
                    } else {
                        // SAFETY: Exclusive access by invariants & null check above.
                        unsafe { (*node.prev).next = node.next };
                    }
                    // Don't need to do anything here if this is at the tail of the
                    // free-list.
                    if !node.next.is_null() {
                        // SAFETY: Exclusive access by invariants & null check above.
                        unsafe { (*node.next).prev = node.prev };
                    }
                    debug_assert_ne!(n_bytes_padding, 0);
                    // SAFETY: Again with the trickery, past me! See
                    // the long comment marked with `[REF 1]`
                    // above. The same argument applies.
                    unsafe {
                        ptr.cast::<u8>()
                            .add(n_bytes_padding)
                            .cast::<usize>()
                            .write(n_bytes_padding);
                    };
                    // SAFETY: The base ptr is valid and we properly
                    // offset to the data.
                    unsafe {
                        ptr.cast::<u8>()
                            .add(n_bytes_padding)
                            .add(mem::size_of::<usize>())
                    }
                }
            }
        };

        debug_assert_eq!(
            ptr as usize % layout.align(),
            0,
            "kalloc::linked_list: allocator allocated unaligned block"
        );

        log::trace!("allocate: layout={layout:?}, ptr={ptr:#p}, self={alloc:#?}");

        debug_assert!(
            alloc.free_list_valid(),
            "LinkedListAlloc::allocate: free list was invalid after allocation {alloc:#?}"
        );

        // SAFETY: Our pointer is non-null (unless something got
        // seriously clobbered, in which case we have bigger
        // problems). It is also properly aligned and of the proper
        // size.
        Ok(unsafe { NonNull::new_unchecked(slice::from_raw_parts_mut(ptr, layout.size())) })
    }

    #[track_caller]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let mut alloc = self.inner.lock();
        debug_assert!(
            alloc.init,
            "LinkedListAlloc::deallocate: kalloc not initialized"
        );
        debug_assert!(
            alloc.free_list_valid(),
            "LinkedListAlloc::deallocate: free list was invalid before deallocation: {alloc:#?}"
        );
        // SAFETY: This pointer is safe to use as it refers to data
        // within the same linked list allocator node (specifically,
        // the node's metadata). Additionally, the base pointer is
        // properly-aligned for usize as our minimum alignment is 8.
        let padding_size_ptr = unsafe { ptr.as_ptr().cast::<usize>().sub(1) };
        // SAFETY: We have exclusive access to this node by our invariants.
        let padding_size = unsafe { *padding_size_ptr };
        // SAFETY: We have still not left the boundary of our node's
        // metadata, making this valid. (This pointer now points to
        // the size tag).
        let node_ptr = unsafe {
            ptr.as_ptr()
                .sub(padding_size)
                .sub(mem::size_of::<usize>())
                .cast::<FreeNode>()
        };

        // SAFETY: The pointer is valid and we have exclusive access
        // to this node by our invariants.
        let node_size = unsafe { node_ptr.cast::<usize>().read() };
        debug_assert_eq!(
            node_size & (1 << 63),
            0,
            "kalloc::linked_list: double free at {ptr:#p}",
        );
        debug_assert!(
            layout.size() <= node_size,
            "kalloc::linked_list: attempted to free a node with a layout larger than the node"
        );

        // if the free list is null, then we need to start the list here
        if alloc.free_list.is_null() {
            alloc.free_list = node_ptr;
            // SAFETY: We have exclusive access to this node by our
            // invariants. The base pointer is valid.
            unsafe {
                addr_of_mut!((*node_ptr).size_tag).write(node_size | (1 << 63));
                addr_of_mut!((*node_ptr).prev).write(ptr::null_mut());
                addr_of_mut!((*node_ptr).next).write(ptr::null_mut());
            }
            //log::trace!("deallocate: ptr={ptr:#p} layout={layout:?} self={alloc:#?}");

            debug_assert!(
                alloc.free_list_valid(),
                "LinkedListAlloc::deallocate: free list was invalid after deallocation (path 1): {alloc:#?}"
            );
            return;
        }
        let mut prev: *mut FreeNode = ptr::null_mut();
        let mut next = alloc.free_list;
        loop {
            // if next is null, we need to add this node to the end of
            // the list.
            if next.is_null() {
                if !prev.is_null() {
                    // SAFETY: By invariants; null check
                    unsafe {
                        (*prev).next = node_ptr;
                    }
                }

                // SAFETY: Exclusive access by invariants.
                unsafe {
                    addr_of_mut!((*node_ptr).size_tag).write(node_size | (1 << 63));
                    addr_of_mut!((*node_ptr).prev).write(prev);
                    addr_of_mut!((*node_ptr).next).write(ptr::null_mut());
                }
                //log::trace!("deallocate: ptr={ptr:#p} layout={layout:?} self={alloc:#?}");

                // todo: coalesce

                debug_assert!(
                    alloc.free_list_valid(),
                    "LinkedListAlloc::deallocate: free list was invalid after deallocation (path 2): {alloc:#?}"
                );
                return;
            }

            // if next > node_ptr, then we've found the right spot
            // (because of our address-ordering invariant), so we need
            // to splice the block into the list here.
            if next > node_ptr {
                if prev.is_null() {
                    alloc.free_list = node_ptr;
                } else {
                    // SAFETY: By invariants; null check
                    unsafe {
                        (*prev).next = node_ptr;
                    }
                }

                // SAFETY: Exclusive access by
                // invariants. Short-circuiting null check for `next`
                // on iteration above.
                unsafe {
                    (*next).prev = node_ptr;
                    addr_of_mut!((*node_ptr).size_tag).write(node_size | (1 << 63));
                    addr_of_mut!((*node_ptr).prev).write(prev);
                    addr_of_mut!((*node_ptr).next).write(next);
                }

                debug_assert!(
                    alloc.free_list_valid(),
                    "LinkedListAlloc::deallocate: free list was invalid after deallocation (path 3): {alloc:#?}"
                );
                return;
            }

            // haven't found our spot yet, continue.
            prev = next;
            // SAFETY: We have exclusive access to all freed nodes by
            // our invariants.
            next = unsafe { &*prev }.next;
        }
    }
}

pub const BLOCK_SPLIT_THRESHOLD: usize = 64 /* bytes */;
