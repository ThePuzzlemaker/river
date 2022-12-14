#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FileHdr {
    pub ident: FileIdent,
    pub filety: u16,
    pub machine: u16,
    pub version: u32,
    pub entry_addr: usize,
    pub prog_hdr_offset: usize,
    pub sec_hdr_offset: usize,
    pub flags: u32,
    pub hdr_size: u16,
    pub prog_hdr_size: u16,
    pub prog_hdr_count: u16,
    pub sec_hdr_size: u16,
    pub sec_hdr_count: u16,
    pub sec_strtab_index: u16,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C, align(16))]
pub struct FileIdent {
    pub magic: [u8; 4],
    pub class: u8,
    pub data: u8,
    pub version: u8,
    pub osabi: u8,
    pub abi_version: u8,
}

pub const MAGIC: [u8; 4] = *b"\x7fELF";
pub const ELFCLASS64: u8 = 2;
pub const ELFDATA2LSB: u8 = 1;
pub const ELFOSABI_SYSV: u8 = 0;
pub const EV_CURRENT: u32 = 1;
pub const EM_RISCV: u16 = 243;
pub const SH_UNDEF: u16 = 0;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct SectionHdr {
    pub name_offset: u32,
    pub sec_type: u32,
    pub flags: u64,
    pub virt_addr: usize,
    pub offset: usize,
    pub size: usize,
    pub link: u32,
    pub info: u32,
    pub addr_align: usize,
    pub entry_size: usize,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ProgramHdr {
    pub seg_type: u32,
    pub flags: u32,
    pub offset: u64,
    pub virt_addr: u64,
    pub phys_addr: u64,
    pub file_size: u64,
    pub mem_size: u64,
    pub align: u64,
}
