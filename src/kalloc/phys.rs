use core::{fmt, mem};

use alloc::collections::VecDeque;

use crate::{
    addr::{DirectMapped, Physical, PhysicalMut},
    paging,
    spin::{SpinMutex, SpinMutexGuard},
    util,
};

// TODO: perhaps find a way to more efficiently fill the available physical
// memory? could probably set pages out-of-range as filled, unfill the rest, and
// have an `end` ptr as well as the existing `base` (or just check against
// `size`)

/// Find the best order for a given size chunk.
#[inline(always)]
pub const fn what_order(size: usize) -> u32 {
    (size.next_multiple_of(4096).next_power_of_two() / 4096).ilog2()
}

/// We currently only support up to 64GiB of physical memory.
pub const TOTAL_MAX_ORDER: u32 = calc_max_order(64 * 1024 * 1024 * 1024);

static PMALLOC: PMAlloc = PMAlloc {
    inner: SpinMutex::new(PMAllocInner::new_uninit()),
};

#[derive(Debug)]
pub struct PMAlloc {
    inner: SpinMutex<PMAllocInner>,
}

impl PMAlloc {
    /// # Panics
    ///
    /// This function will panic if the global `PMAlloc` has not been initialized.
    /// See [`PMAlloc::init`].
    #[track_caller]
    pub fn get() -> PMAllocLock<'static> {
        let pma = PMALLOC.inner.lock();
        assert!(pma.init, "PMAlloc::get: not initialized");
        pma
    }

    /// # Safety
    ///
    /// - `base` must be page-aligned.
    /// - `base` must be non-null
    /// - `size` must be a power of two.
    /// - `size` must be a multiple of 4096.
    #[cfg_attr(debug_assertions, track_caller)]
    pub unsafe fn init(base: PhysicalMut<u8, DirectMapped>, size: usize) {
        debug_assert!(!base.is_null(), "PMAlloc::init: base must be non-null");
        debug_assert!(
            base.is_page_aligned(),
            "PMAlloc::init: base must be page-aligned"
        );
        debug_assert!(
            size.is_power_of_two(),
            "PMAlloc::init: size must be a power of 2"
        );
        debug_assert_eq!(
            size % 4096,
            0,
            "PMAlloc::init: size must be a multiple of 4096"
        );
        let mut pma = PMALLOC.inner.lock();
        debug_assert!(
            !pma.init,
            "PMAlloc::init: pma must not already be initialized"
        );
        pma.init = true;
        pma.base = base;
        pma.max_order = calc_max_order(size);
        pma.bitree = Bitree {
            base: base.cast(),
            n_bits: calc_bitree_bits(pma.max_order),
            max_order: pma.max_order,
        };
        let bitree_n_pages = calc_bitree_pages(pma.max_order);
        pma.size = size;
        for page_idx in 0..bitree_n_pages {
            // SAFETY: Our caller guarantees that that this area is safe to modify.
            unsafe { pma.mark_used(base.cast().add(4096 * page_idx), 0) };
        }
    }
}

#[derive(Debug)]
pub struct PMAllocInner {
    init: bool,
    bitree: Bitree,
    base: PhysicalMut<u8, DirectMapped>,
    max_order: u32,
    size: usize,
}

type PMAllocLock<'a> = SpinMutexGuard<'a, PMAllocInner>;

impl PMAllocInner {
    const fn new_uninit() -> Self {
        PMAllocInner {
            init: false,
            bitree: Bitree {
                base: Physical::null(),
                n_bits: 0,
                max_order: 0,
            },
            base: Physical::null(),
            max_order: 0,
            size: 0,
        }
    }

    pub fn get_max_order(&self) -> u32 {
        self.max_order
    }

    /// Mark a chunk as used, without actually allocating it.
    ///
    /// # Safety
    ///
    /// This function is wildly unsafe. Use with caution. Proper safety
    /// documentation coming soon™
    ///
    /// # Panics
    ///
    /// This function will panic if the page is outside the range of this
    /// allocator.
    pub unsafe fn mark_used(&mut self, chunk: PhysicalMut<u8, DirectMapped>, order: u32) {
        let alloc_off = (chunk.into_usize() - self.base.into_usize()) / (4096 * (1 << order));
        let ix = self.bitree.ix_of(alloc_off, order).unwrap();
        // Set the bit of this order and all above
        self.bitree.set(ix, true);
        self.set_above(ix);
    }

    fn set_above(&mut self, ix: usize) {
        let mut loop_ix = self.bitree.parent(ix);
        // Set the bit of all the orders above the provided order
        while let Some(ix) = loop_ix {
            // Break if the bit was already set
            if self.bitree.set(ix, true) {
                break;
            }
            loop_ix = self.bitree.parent(ix);
        }
    }

    /// Allocate a (2^order * 4096)-large chunk.
    ///
    /// # Panics
    ///
    /// This function will panic if there is not enough space in the
    /// allocator.
    ///
    /// TODO: make this not panic, if possible
    pub fn allocate(&mut self, order: u32) -> Option<PhysicalMut<u8, DirectMapped>> {
        if order > TOTAL_MAX_ORDER {
            return None;
        }

        let ix_base = self.bitree.ix_of_first(order);
        let n_nodes = self.bitree.num_nodes(order);
        for ix in ix_base..ix_base + n_nodes {
            // Set the bit. Continue if it was already set.
            if self.bitree.set(ix, true) {
                continue;
            }
            // The bit was not already set, so we have our chunk.
            // We need to make sure the blocks above are split, iff necessary.
            self.set_above(ix);

            let alloc_off = self.bitree.alloc_off_of(ix).unwrap();
            let offset_addr = alloc_off * 4096 * (1 << order);
            let addr = offset_addr + self.base.into_usize();
            return Physical::try_from_usize(addr);
        }
        None
    }

    /// Returns the number of free pages in the allocator.
    ///
    /// # Panics
    ///
    /// This function should not panic unless the allocator's internal
    /// state is invalid.
    pub fn num_free_pages(&self) -> u64 {
        fn helper(bitree: &Bitree, n_pgs: &mut u64, ix: usize) {
            if bitree.get(ix) {
                if let Some(ix_left) = bitree.left_child(ix) {
                    helper(bitree, n_pgs, ix_left);
                }
                if let Some(ix_right) = bitree.right_child(ix) {
                    helper(bitree, n_pgs, ix_right);
                }
            } else {
                *n_pgs += 1 << bitree.order_of(ix).unwrap();
            }
        }

        let mut n_pgs = 0;

        helper(
            &self.bitree,
            &mut n_pgs,
            self.bitree.ix_of_first(self.max_order),
        );
        n_pgs
    }

    /// Deallocate a (2^order * 4096)-large chunk.
    ///
    /// # Safety
    ///
    /// - The pointer provided must have been allocated by
    ///   [`PMAllocInner::allocate`].
    /// - The order provided must be the same as the one used to allocate this
    ///   chunk.
    ///
    /// # Panics
    ///
    /// This function will panic if the provided order was larger than this
    /// allocator can handle.
    #[track_caller]
    pub unsafe fn deallocate(&mut self, chunk: PhysicalMut<u8, DirectMapped>, order: u32) {
        assert!(
            order <= TOTAL_MAX_ORDER,
            "PMAlloc::deallocate: order was too large: chunk={:?}, order={:?}",
            chunk,
            order
        );

        let alloc_off = (chunk.into_usize() - self.base.into_usize()) / (4096 * (1 << order));
        let ix = self
            .bitree
            .ix_of(alloc_off, order)
            .expect("PMAlloc::deallocate: could not find ix of chunk");

        // Deallocate this node
        assert!(
            self.bitree.set(ix, false),
            "PMAlloc::deallocate: double free: chunk={:?}, order={:?}",
            chunk,
            order
        );

        let mut loop_sibling = self.bitree.sibling(ix);
        while let Some(sibling) = loop_sibling {
            // Can't coalesce, sibling was allocated.
            if self.bitree.get(sibling) {
                return;
            }

            // We can coalesce.
            if let Some(parent) = self.bitree.parent(sibling) {
                // Unset the parent.
                self.bitree.set(parent, false);
                // Continue the loop with the parent's sibling.
                loop_sibling = self.bitree.sibling(parent);
            } else {
                // We're at the root node, so we can't coalesce.
                return;
            }
        }
    }
}

// SAFETY: PMAlloc's inner data is `SpinMutex` protected.
unsafe impl Send for PMAlloc {}
// SAFETY: See above.
unsafe impl Sync for PMAlloc {}

#[inline(always)]
const fn calc_max_order(size: usize) -> u32 {
    (size / 4096).ilog2()
}

#[inline(always)]
fn calc_bitree_pages(max_order: u32) -> usize {
    util::round_up_pow2(calc_bitree_bits(max_order).div_ceil(8), 4096)
}

#[inline(always)]
fn calc_bitree_bits(max_order: u32) -> usize {
    2 * (1 << max_order) - 1
}

// Some important invariants of the bitree:
// - 0-based
// - Breadth-first
// - Siblings are adjacent
// - L child ixs are odd
// - R child ixs are even

struct Bitree {
    base: PhysicalMut<usize, DirectMapped>,
    n_bits: usize,
    max_order: u32,
}

impl fmt::Debug for Bitree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bitree")
            .field("base", &self.base)
            .field("n_bits", &self.n_bits)
            .field(
                "data",
                &BitreeDataWrapper {
                    base: self.base,
                    len: self.n_bits / 8,
                    pos: 0,
                },
            )
            .finish()
    }
}

#[derive(Copy, Clone)]
struct AlwaysBinary(usize);
impl fmt::Debug for AlwaysBinary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#b}", self.0)
    }
}

#[derive(Copy, Clone)]
struct BitreeDataWrapper {
    base: PhysicalMut<usize, DirectMapped>,
    len: usize,
    pos: usize,
}
impl fmt::Debug for BitreeDataWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(*self).finish()
    }
}

#[allow(clippy::copy_iterator)]
impl Iterator for BitreeDataWrapper {
    type Item = AlwaysBinary;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.len {
            return None;
        }
        let phys = self.base.add(self.pos * mem::size_of::<usize>());
        let virt = if paging::enabled() {
            phys.into_virt().into_identity()
        } else {
            phys.into_identity().into_virt()
        };
        // SAFETY: This is valid due to the invariants of the Bitree we were created of
        let res = unsafe { virt.into_ptr().read() };

        self.pos += 1;

        Some(AlwaysBinary(res))
    }
}

impl Bitree {
    #[track_caller]
    pub fn get(&self, ix: usize) -> bool {
        assert!(
            ix < self.n_bits,
            "Bitree::get: ix out of bounds: ix={:?}, len={:?}",
            ix,
            self.n_bits
        );
        let word = ix / 64;
        let bit = ix % 64;
        let phys = self.base.add(word * mem::size_of::<usize>());
        let virt = if paging::enabled() {
            phys.into_virt().into_identity()
        } else {
            phys.into_identity().into_virt()
        };
        // SAFETY: This is valid due to our invariants.
        let word_val = unsafe { virt.into_ptr().read() };
        word_val & (1 << bit) != 0
    }

    #[track_caller]
    pub fn set(&mut self, ix: usize, val: bool) -> bool {
        assert!(
            ix < self.n_bits,
            "Bitree::get: ix out of bounds: ix={:?}, len={:?}",
            ix,
            self.n_bits
        );
        let word = ix / 64;
        let bit = ix % 64;
        let phys = self.base.add(word * mem::size_of::<usize>());
        let virt = if paging::enabled() {
            phys.into_virt().into_identity()
        } else {
            phys.into_identity().into_virt()
        };
        // SAFETY: This is valid due to our invariants.
        let word_ref = unsafe { &mut *virt.into_ptr_mut() };
        let flag = usize::from(val) << bit;
        let old = *word_ref;
        // clear bit
        *word_ref &= !(1 << bit);
        // set bit
        *word_ref |= flag;
        old & (1 << bit) != 0
    }

    #[allow(dead_code)]
    pub fn left_child(&self, ix: usize) -> Option<usize> {
        let ix = 2 * ix + 1;
        if ix >= self.n_bits {
            return None;
        }
        Some(ix)
    }

    #[allow(dead_code)]
    pub fn right_child(&self, ix: usize) -> Option<usize> {
        let ix = 2 * ix + 2;
        if ix >= self.n_bits {
            return None;
        }
        Some(ix)
    }

    pub fn parent(&self, ix: usize) -> Option<usize> {
        if ix == 0 {
            return None;
        }
        let ix = (ix - 1) / 2;
        if ix >= self.n_bits {
            return None;
        }
        Some(ix)
    }

    pub fn sibling(&self, ix: usize) -> Option<usize> {
        if ix == 0 {
            return None;
        }
        let ix = if ix % 2 == 0 { ix - 1 } else { ix + 1 };
        if ix >= self.n_bits {
            return None;
        }
        Some(ix)
    }

    /// Get the index of the `alloc_off`'th node in the order'th level of the tree, bottom-up
    pub fn ix_of(&self, alloc_off: usize, order: u32) -> Option<usize> {
        let ix_base = self.ix_of_first(order);
        // add in the offset
        let ix = ix_base + alloc_off;
        if ix >= self.n_bits {
            return None;
        }
        Some(ix)
    }

    /// Get the index of the first node in the order'th level of the tree, bottom-up
    pub fn ix_of_first(&self, order: u32) -> usize {
        (1 << (self.max_order - order)) - 1
    }

    /// Get the `alloc_off` of the ix'th node
    pub fn alloc_off_of(&self, ix: usize) -> Option<usize> {
        let order = self.order_of(ix)?;
        let ix_base = self.ix_of_first(order);
        // subtract the base from the ix
        Some(ix - ix_base)
    }

    /// Get the order of the ix'th node.
    #[inline]
    pub fn order_of(&self, ix: usize) -> Option<u32> {
        if ix == usize::MAX || ix >= self.n_bits {
            return None;
        }
        Some(self.max_order - (ix + 1).ilog2())
    }

    /// Get the number of nodes in the order'th level of the tree, bottom-up
    fn num_nodes(&self, order: u32) -> usize {
        1 << (self.max_order - order)
    }
}
