#![no_std]

#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct BootFsHeader {
    pub magic: u32,
    pub n_entries: u32,
}
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct BootFsEntry {
    pub name_offset: u32,
    pub name_length: u32,
    pub offset: u32,
    pub length: u32,
}
