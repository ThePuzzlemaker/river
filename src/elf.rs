use core::mem;

use alloc::vec;
use alloc::vec::Vec;
use bitflags::bitflags;
use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    io_traits::{Read, Seek, SeekFrom},
    println,
};

use self::raw::FileIdent;

/// This module contains an opinionated parser for RISC-V ELF64
/// files. The structures contained in this file are intended to be
/// consumed idiomatically, and do not necessarily represent ELF files
/// one-to-one, though this may change in the future.
///
/// # References
///
/// This module's documentation will make heavy uses of the following
/// references:
///
/// - [RISC-V ABIs Specification
/// v1.0](https://github.com/riscv-non-isa/riscv-elf-psabi-doc/releases/download/v1.0/riscv-abi.pdf).
/// - [ELF-64 Object File Format v1.5 Draft
/// 2](https://uclibc.org/docs/elf-64-gen.pdf).
///
/// However, to prevent the documentation source from being too
/// cluttered, links to these sources will only be present in the
/// module-level documentation.

/// The `e_machine` type for RISC-V. **All** RISC-V executables use
/// this value.[^1]
///
/// [^1]: RISC-V ABIs Specification, section 8.1, subheading
/// `e_machine`.
pub const MACHINE_TYPE_RISCV: u16 = 243;

#[derive(Debug)]
pub struct Elf {
    pub filety: FileType,
    pub entry_addr: usize,
    pub section_headers: Vec<SectionHdr>,
    sec_hdr_offset: usize,
    sec_hdr_count: u16,
    sec_strtab_index: u16,
}

pub mod raw;

impl Elf {
    /// # Errors
    ///
    /// TODO
    pub fn parse_header<R: Read + Seek>(reader: &mut R) -> Result<Self, ParseError> {
        let mut elf = Self {
            filety: FileType::None,
            entry_addr: 0,
            section_headers: vec![],
            sec_hdr_offset: 0,
            sec_hdr_count: 0,
            sec_strtab_index: 0,
        };

        let mut buf = [0; mem::size_of::<raw::FileHeader>()];
        reader.read_exact(&mut buf).map_err(|_| ParseError)?;

        // SAFETY: `FileHeader` is valid in any bit pattern, and has
        // the same size.
        let file_hdr = unsafe { mem::transmute::<_, raw::FileHeader>(buf) };

        let proper_ident = FileIdent {
            magic: raw::MAGIC,
            class: raw::ELFCLASS64,
            data: raw::ELFDATA2LSB,
            version: raw::EV_CURRENT as u8,
            osabi: raw::ELFOSABI_SYSV,
            abi_version: 0,
        };
        if file_hdr.ident != proper_ident {
            return Err(ParseError);
        }

        elf.filety = FileType::try_from(file_hdr.filety).map_err(|_| ParseError)?;
        elf.entry_addr = file_hdr.entry_addr;
        elf.sec_hdr_offset = file_hdr.sec_hdr_offset;
        elf.sec_hdr_count = file_hdr.sec_hdr_count;
        elf.sec_strtab_index = file_hdr.sec_strtab_index;

        if file_hdr.version != raw::EV_CURRENT
            || file_hdr.hdr_size != mem::size_of::<raw::FileHeader>() as u16
            || file_hdr.prog_hdr_size != 56
            || file_hdr.sec_hdr_size != mem::size_of::<raw::SectionHdr>() as u16
        {
            return Err(ParseError);
        }

        Ok(elf)
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn parse_section_headers<R: Read + Seek>(
        &mut self,
        reader: &mut R,
    ) -> Result<(), ParseError> {
        reader
            .seek(SeekFrom::Start(self.sec_hdr_offset as u64))
            .map_err(|_| ParseError)?;

        for _ in 0..self.sec_hdr_count {
            let section_hdr = SectionHdr::parse(reader)?;
            self.section_headers.push(section_hdr);
        }

        Ok(())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u16)]
pub enum FileType {
    None = 0,
    Rel = 1,
    Exec = 2,
    Dyn = 3,
    Core = 4,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ParseError;

#[derive(Debug)]
pub struct SectionHdr {
    name_offset: u32,
    pub sec_type: SectionType,
    pub flags: SectionFlags,
    pub virt_addr: usize,
    pub offset: usize,
    pub size: usize,
    pub addr_align: usize,
    pub ent_size: usize,
    pub info: u32,
    pub link: u32,
}

impl SectionHdr {
    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn parse<R: Read + Seek>(reader: &mut R) -> Result<Self, ParseError> {
        let mut sec = SectionHdr {
            name_offset: 0,
            sec_type: SectionType::Null,
            flags: SectionFlags::empty(),
            virt_addr: 0,
            offset: 0,
            size: 0,
            addr_align: 0,
            ent_size: 0,
            info: 0,
            link: 0,
        };

        let mut buf = [0; mem::size_of::<raw::SectionHdr>()];
        reader.read_exact(&mut buf).map_err(|_| ParseError)?;

        // SAFETY: `SectionHdr` is valid in any bit pattern, and has
        // the same size.
        let sec_hdr = unsafe { mem::transmute::<_, raw::SectionHdr>(buf) };
        sec.name_offset = sec_hdr.name_offset;
        sec.sec_type = SectionType::try_from(sec_hdr.sec_type).map_err(|_| ParseError)?;
        sec.flags = SectionFlags::from_bits_truncate(sec_hdr.flags);
        sec.virt_addr = sec_hdr.virt_addr;
        sec.offset = sec_hdr.offset;
        sec.size = sec_hdr.size;
        sec.addr_align = sec_hdr.addr_align;
        sec.ent_size = sec_hdr.entry_size;
        sec.info = sec_hdr.info;
        sec.link = sec_hdr.link;

        if !sec_hdr.addr_align.is_power_of_two()
            && (sec_hdr.addr_align != 0 || sec.sec_type != SectionType::Null)
        {
            return Err(ParseError);
        }

        Ok(sec)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u32)]
pub enum SectionType {
    Null = 0,
    ProgBits = 1,
    SymTab = 2,
    StrTab = 3,
    Rela = 4,
    Hash = 5,
    Dynamic = 6,
    Note = 7,
    NoBits = 8,
    Rel = 9,
    ShLib = 10,
    DynSym = 11,
    RVAttributes = 0x7000_0003,
}

// rustfmt keeps messing this up idk why
#[rustfmt::skip]
bitflags! {
    #[repr(transparent)]
    pub struct SectionFlags: u64 {
	const WRITE = 1;
	const ALLOC = 2;
	const EXEC_INSTR = 4;
	const MASK_OS = 0x0F00_0000;
	const MASK_PROC = 0xF000_0000;
    }
}
