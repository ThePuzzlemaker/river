use bitfield::bitfield;
use rille::capability::CapabilityType;

use rille::addr::{DirectMapped, Ppn, VirtualMut, Vpns};

#[repr(C)]
#[derive(Copy, Clone)]
pub union CapabilitySlot {
    empty: EmptySlot,
    captbl: CaptblSlot,
    untyped: UntypedSlot,
    pgtbl: PgTblSlot,
    page: PageSlot,
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone, Debug)]
    pub struct EmptySlot(u128);
    pub u8, from into CapabilityType, cap_type, set_cap_type: 4, 0;
}

impl Default for EmptySlot {
    fn default() -> Self {
        let mut x = Self(0);
        x.set_cap_type(CapabilityType::Empty);
        x
    }
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone, Debug)]
    pub struct PageSlot(u128);
    pub u8, from into CapabilityType, cap_type, set_cap_type: 4, 0;
    pub u64, from into Ppn, phys_addr, set_phys_addr: 48, 5;
    pub u8, size_bits, set_size_bits: 53, 49;
}

impl Default for PageSlot {
    fn default() -> Self {
        let mut x = Self(0);
        x.set_cap_type(CapabilityType::Page);
        x
    }
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone, Debug)]
    pub struct PgTblSlot(u128);
    pub u8, from into CapabilityType, cap_type, set_cap_type: 4, 0;
    pub u32, from into Vpns, virt_addr, set_virt_addr: 31, 5;
}

impl Default for PgTblSlot {
    fn default() -> Self {
        let mut x = Self(0);
        x.set_cap_type(CapabilityType::PgTbl);
        x
    }
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone, Debug)]
    pub struct CaptblSlot(u128);
    pub u8, from into CapabilityType, cap_type, set_cap_type: 4, 0;
    pub u64, from into VirtualMut<[u8; 32], DirectMapped>, virt_addr, set_virt_addr: 43, 5;
    pub u8, size_bits, set_size_bits: 48, 44;
}

impl Default for CaptblSlot {
    fn default() -> Self {
        let mut x = Self(0);
        x.set_cap_type(CapabilityType::Captbl);
        x
    }
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone, Debug)]
    pub struct UntypedSlot(u128);
    pub u8, from into CapabilityType, cap_type, set_cap_type: 4, 0;
    pub u64, from into VirtualMut<u8, DirectMapped>, base_virt_addr, set_base_virt_addr: 43, 5;
    pub u64, from into VirtualMut<u8, DirectMapped>, free_virt_addr, set_free_virt_addr: 82, 44;
}

impl Default for UntypedSlot {
    fn default() -> Self {
        let mut x = Self(0);
        x.set_cap_type(CapabilityType::Untyped);
        x
    }
}
