//! This module defines capabilities related to paging, and their
//! operations.
use core::{convert::Infallible, fmt::Debug, marker::PhantomData};

use bitflags::bitflags;

use super::{CapResult, Capability, CapabilityType, Captr, Vpn};

/// The page capability corresponds to a page of physical memory that
/// can be mapped into a page table. The [`PagingLevel`] determines
/// the size of the page (see that type's documentation for more
/// information). Note that, as the paging level determines the size
/// of the object, the paging level cannot be changed after creation.
///
/// See [`PageCaptr`] for operations on this capability.
#[derive(Copy, Clone, Debug)]
pub struct Page<L: PagingLevel>(#[doc(hidden)] PhantomData<L>, #[doc(hidden)] Infallible);

/// Helper type for [`Page`] captrs.
pub type PageCaptr<L> = Captr<Page<L>>;

impl<L: PagingLevel> Capability for Page<L> {
    /// [`PagingLevel::PAGE_SIZE_LOG2`] gives us the bit-size of this
    /// retype operation.
    type RetypeSizeSpec = ();

    fn retype_size_spec(_spec: Self::RetypeSizeSpec) -> usize {
        L::PAGE_SIZE_LOG2
    }

    const CAPABILITY_TYPE: CapabilityType = CapabilityType::Page;
}

/// The page table capability corresponds to a page table that can be
/// used for address translation. The [`PagingLevel`] determines which
/// part of the address translation hierarchy this page table
/// occupies.
///
/// While the kernel's representation of page tables at all paging
/// levels is identical, `rille` purposefully has these as different
/// types in order to aide in ensuring that levels are properly
/// accounted for. However, for utilities such as
/// [`Captr<Untyped>::retype_many`], implementations of [`From`] and
/// [`Into`] are provided for ease-of-use. These operations are
/// extremely cheap (in fact, they are practically no-ops from a
/// processor perspective).
///
/// See [`PgTblCaptr`] for operations on this capability.
#[derive(Copy, Clone, Debug)]
pub struct PageTable<L: PagingLevel>(#[doc(hidden)] PhantomData<L>, #[doc(hidden)] Infallible);

/// Helper type for [`PageTable`] captrs.
pub type PgTblCaptr<L> = Captr<PageTable<L>>;

/// Page tables are divided into different "levels", where each level
/// determines which part of the address translation hierarchy the
/// page/page table corresponds to.
///
/// On RISC-V, with the Sv39 paging strategy, there are three levels:
/// - [`BasePage`]: Level 0, 4KiB (`2 << 12` bytes) in size
/// - [`MegaPage`]: Level 1, 2MiB (`2 << 21` bytes) in size
/// - [`GigaPage`]: Level 2, 1GiB (`2 << 30` bytes) in size
///
/// The paging level determines the smallest unit of memory that a
/// given page table can map. Each level can address a maximum of 512
/// (`2 << 9`) pages.
///
/// Note that the upper half of memory (`0xFFFF_FFC0_0000_0000` to
/// `0xFFFF_FFFF_FFFF_FFFF`, inclusive)[^1] is reserved for kernel
/// use. This leaves the memory range `0x0000_0000_0000_0000` to
/// `0x0000_0020_0000_0000` for use by userspace applications. This is
/// certainly enough for all applications that river will be used for,
/// being almost 128GiB of virtual memory.
///
/// [^1]: Addresses in river are canonicalized, meaning that if the
///   topmost bit of "usable" address space is set, the rest of the
///   bits must be set. i.e.:
///   ```ignore
///   if addr & (1 << 38) != 0 {
///       addr |= usize::MAX << 38
///   }
///   ```
///   Note that we use Sv39 (i.e., the topmost usable bit is bit 38,
///   the 39'th bit).
pub trait PagingLevel: Copy + Clone + Debug + super::private::Sealed {
    /// The base-2 logarithm of this paging level's page size in
    /// bytes.
    const PAGE_SIZE_LOG2: usize;
}

/// Gigapages are 1GiB (`2 << 30` bytes) in size. This is the base
/// unit of the level 0 page table.
#[derive(Copy, Clone, Debug)]
pub struct GigaPage(#[doc(hidden)] Infallible);
impl PagingLevel for GigaPage {
    const PAGE_SIZE_LOG2: usize = MegaPage::PAGE_SIZE_LOG2 + 9;
}

/// Megapages are 2MiB (`2 << 21` bytes) in size. This is the base
/// unit of the level 1 page table.
#[derive(Copy, Clone, Debug)]
pub struct MegaPage(#[doc(hidden)] Infallible);
impl PagingLevel for MegaPage {
    const PAGE_SIZE_LOG2: usize = BasePage::PAGE_SIZE_LOG2 + 9;
}

/// Base pages are 4KiB (`2 << 12` bytes) in size. This is the base
/// unit of the level 2 page table.
#[derive(Copy, Clone, Debug)]
pub struct BasePage(#[doc(hidden)] Infallible);
impl PagingLevel for BasePage {
    const PAGE_SIZE_LOG2: usize = 12;
}

impl<L: PagingLevel> Capability for PageTable<L> {
    /// Page table sizes are constant, no matter the [`PagingLevel`].
    type RetypeSizeSpec = ();

    fn retype_size_spec(_spec: Self::RetypeSizeSpec) -> usize {
        0
    }

    const CAPABILITY_TYPE: CapabilityType = CapabilityType::PgTbl;
}

#[rustfmt::skip]
bitflags! {
    /// Flags corresponding to access rights for a page by any process
    /// using the page table.
    pub struct PageTableFlags: u8 {
	/// This page is readable, i.e. it can be used for load
	/// instructions.
        const READ = 1 << 1;
	/// This page is writable, i.e. it can be used for store
	/// instructions.
        const WRITE = 1 << 2;
	/// This page is executable, i.e. it can be used for
	/// instruction fetches.
        const EXECUTE = 1 << 3;
	/// This page is readable and writable.
        const RW = Self::READ.bits | Self::WRITE.bits;
	/// This page is readable and executable.
        const RX = Self::READ.bits | Self::EXECUTE.bits;
	/// This page is readable, writable, and executable.
	const RWX = Self::READ.bits | Self::WRITE.bits | Self::EXECUTE.bits;
    }
}

impl PgTblCaptr<BasePage> {
    /// Map a level 2 page table into a level 1 page table. This makes
    /// the processor use the L2 table for translating the 1MiB region
    /// specified by the [`Vpn`] in the L1 table.
    ///
    /// # Errors
    ///
    /// TODO
    pub fn map(
        self: PgTblCaptr<BasePage>,
        _into: PgTblCaptr<MegaPage>,
        _vpn: Vpn,
        _flags: PageTableFlags,
    ) -> CapResult<()> {
        todo!();
    }
}

impl PgTblCaptr<MegaPage> {
    /// Map a level 1 page table into a level 0 page table. This makes
    /// the processor use the L1 table for translating the 1GiB region
    /// specified by the [`Vpn`] in the L0 table.
    ///
    /// # Limitations
    ///
    /// The [`Vpn`] must correspond to the lower half of virtual
    /// memory, i.e. it cannot be `128` or higher.
    ///
    /// # Errors
    ///
    /// TODO
    pub fn map(
        self: PgTblCaptr<MegaPage>,
        _into: PgTblCaptr<GigaPage>,
        _vpn: Vpn,
        _flags: PageTableFlags,
    ) -> CapResult<()> {
        todo!();
    }
}

impl<L: PagingLevel> PageCaptr<L> {
    /// Map a page of a given level into its corresponding page
    /// table. This makes the processor use the provided physical page
    /// for the region specified by the [`Vpn`] in the table.
    ///
    /// # Limitations
    ///
    /// If the [`PagingLevel`] is [`GigaPage`] (L0 table), the [`Vpn`]
    /// must correspond to the lower half of virtual memory, i.e. it
    /// cannot be `128` or higher.
    ///
    /// # Errors
    ///
    /// TODO
    pub fn map(
        self: PageCaptr<L>,
        _into: PgTblCaptr<L>,
        _vpn: Vpn,
        _flags: PageTableFlags,
    ) -> CapResult<()> {
        todo!()
    }
}

impl<L: PagingLevel> PgTblCaptr<L> {
    /// Unmap the region specified by the [`Vpn`] in this page table.
    ///
    /// # Errors
    ///
    /// TODO
    pub fn unmap(self, _vpn: Vpn) -> CapResult<()> {
        todo!()
    }
}

/// Internal macro that implements [`Into`]/[`From`] for [`PageTable<L>`]
macro_rules! impl_relevel {
    ($captr:ident, $pgtbl:ident => [$($first:ident, $second:ident;)+]) => {
        $(
	    impl ::core::convert::From<$captr<$pgtbl<$first>>> for $captr<$pgtbl<$second>> {
		fn from(x: $captr<$pgtbl<$first>>) -> Self {
		    // SAFETY: The representation of
		    // Captr<PageTable<Ln>> and Captr<PageTable<Lm>>
		    // is the same for all Ln and Lm.
		    unsafe { ::core::mem::transmute(x) }
		}
            }
	)+
    };
}

impl_relevel! {
    Captr, PageTable => [
  /*GigaPage, GigaPage;*/ GigaPage, MegaPage;   GigaPage, BasePage;
    MegaPage, GigaPage; /*MegaPage, MegaPage;*/ MegaPage, BasePage;
    BasePage, GigaPage;   BasePage, MegaPage; /*BasePage, BasePage;*/
    ]
}
