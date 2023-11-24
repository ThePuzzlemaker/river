//! This module contains an opinionated parser for RISC-V ELF64
//! files. The structures contained in this file are intended to be
//! consumed idiomatically, and do not necessarily represent ELF files
//! one-to-one, though this may change in the future.
//!
//! # References
//!
//! This module's documentation will make heavy uses of the following
//! references:
//!
//! - [RISC-V ABIs Specification
//! v1.0](https://github.com/riscv-non-isa/riscv-elf-psabi-doc/releases/download/v1.0/riscv-abi.pdf).
//! - [ELF-64 Object File Format v1.5 Draft
//! 2](https://uclibc.org/docs/elf-64-gen.pdf).
//!
//! However, to prevent the documentation source from being too
//! cluttered, links to these sources will only be present in the
//! module-level documentation.
use core::mem;

use bitflags::bitflags;
use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::io_traits::{Read, Seek, SeekFrom};

use self::raw::FileIdent;

/// The `e_machine` type for RISC-V. **All** RISC-V executables use
/// this value.[^1]
///
/// [^1]: RISC-V ABIs Specification, section 8.1, subheading
/// `e_machine`.
pub const MACHINE_TYPE_RISCV: u16 = 243;

/// An ELF file.
#[derive(Debug)]
pub struct Elf {
    /// What type of ELF file is this?
    pub filety: FileType,
    /// What is the entry address of this ELF file?
    pub entry_addr: usize,
    sec_hdr_offset: usize,
    sec_hdr_count: u16,
    sec_strtab_index: u16,
    prog_hdr_offset: usize,
    prog_hdr_count: u16,
}

pub mod raw;

impl Elf {
    /// # Errors
    ///
    /// This will return an error if any read or seek operation
    /// failed, or if the header was invalid.
    pub fn parse_header<R: Read + Seek>(reader: &mut R) -> Result<Self, ParseError> {
        let mut elf = Self {
            filety: FileType::None,
            entry_addr: 0,
            sec_hdr_offset: 0,
            sec_hdr_count: 0,
            sec_strtab_index: 0,
            prog_hdr_offset: 0,
            prog_hdr_count: 0,
        };

        let mut buf = [0; mem::size_of::<raw::FileHdr>()];
        reader.read_exact(&mut buf).map_err(|_| ParseError)?;

        // SAFETY: `FileHeader` is valid in any bit pattern, and has
        // the same size.
        let file_hdr = unsafe { mem::transmute::<_, raw::FileHdr>(buf) };

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
        elf.prog_hdr_offset = file_hdr.prog_hdr_offset;
        elf.prog_hdr_count = file_hdr.prog_hdr_count;

        if file_hdr.version != raw::EV_CURRENT
            || file_hdr.hdr_size != mem::size_of::<raw::FileHdr>() as u16
            || file_hdr.prog_hdr_size != 56
            || file_hdr.sec_hdr_size != mem::size_of::<raw::SectionHdr>() as u16
        {
            return Err(ParseError);
        }

        Ok(elf)
    }

    /// Parse sections into an iterator.
    ///
    /// # Errors
    ///
    /// This function will return an error if any read or seek
    /// operation failed. The inner iterator will contain an error if
    /// parsing a section header failed.
    pub fn parse_sections<'r, R: Read + Seek>(
        &mut self,
        reader: &'r mut R,
    ) -> Result<impl Iterator<Item = Result<Section, ParseError>> + 'r, ParseError> {
        reader
            .seek(SeekFrom::Start(self.sec_hdr_offset as u64))
            .map_err(|_| ParseError)?;

        Ok((0..self.sec_hdr_count).map(|_| Section::parse(reader)))
    }

    /// Parse segments into an iterator.
    ///
    /// # Errors
    ///
    /// This function will return an error if any read or seek
    /// operation failed. The inner iterator will contain an error if
    /// parsing a program header failed.
    pub fn parse_segments<'r, R: Read + Seek>(
        &mut self,
        reader: &'r mut R,
    ) -> Result<impl Iterator<Item = Result<Segment, ParseError>> + 'r, ParseError> {
        reader
            .seek(SeekFrom::Start(self.prog_hdr_offset as u64))
            .map_err(|_| ParseError)?;

        Ok((0..self.prog_hdr_count).map(|_| Segment::parse(reader)))
    }
}

/// What type of ELF file is this?
#[derive(Copy, Clone, Debug, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u16)]
pub enum FileType {
    /// Unknown file type.
    None = 0,
    /// Relocatable file.
    Rel = 1,
    /// Executable file.
    Exec = 2,
    /// Shared object.
    Dyn = 3,
    /// Core dump file.
    Core = 4,
}

/// A struct representing a parse error. At the moment, this has no
/// information on what error occured.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ParseError;

/// An ELF section.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Section {
    name_offset: u32,
    /// What type of section is this?
    pub sec_type: SectionType,
    /// Flags relating to this section.
    pub flags: SectionFlags,
    /// The virtual address of this section.
    pub virt_addr: usize,
    /// The offset of this section into the file.
    pub offset: usize,
    /// The size of this section in bytes.
    pub size: usize,
    /// The alignment of this section. Zero and one both refer to no
    /// alignment.
    pub addr_align: usize,
    /// For fixed-size entry tables, this contains the size in bytes
    /// of each entry.
    pub ent_size: usize,
    /// The interpretation of `info` depends on the section type.
    pub info: u32,
    /// The interpretation of `link` depends on the section type.
    pub link: u32,
}

impl Section {
    /// Try to parse a section.
    ///
    /// # Errors
    ///
    /// This function will return an error if any read or seek
    /// operation failed, or if the section header was invalid.
    pub fn parse<R: Read + Seek>(reader: &mut R) -> Result<Self, ParseError> {
        let mut sec = Section {
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

/// What type of section is this?
#[derive(Copy, Clone, Debug, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u32)]
pub enum SectionType {
    /// Inactive.
    Null = 0,
    /// Program data; the interpretation depends on the type of file.
    ProgBits = 1,
    /// Symbol table.
    SymTab = 2,
    /// String table.
    StrTab = 3,
    /// Relocation entries.
    Rela = 4,
    /// Symbol hash table for dynamic linking.
    Hash = 5,
    /// Information for dynamic linking.
    Dynamic = 6,
    /// Notes.
    Note = 7,
    /// Occupies no space in the file, but is otherwise equivalent to
    /// [`Self::ProgBits`].
    NoBits = 8,
    /// Relocation offsets.
    Rel = 9,
    /// Reserved.
    ShLib = 10,
    /// Dynamic linking symbols.
    DynSym = 11,
    /// Attributes of RISC-V ELF files
    RVAttributes = 0x7000_0003,
}

// rustfmt keeps messing this up idk why
#[rustfmt::skip]
bitflags! {
    /// Section flags.
    #[repr(C)]
    pub struct SectionFlags: u64 {
	/// Writable at runtime.
	const WRITE = 1;
	/// Occupies memory during runtime.
	const ALLOC = 2;
	/// Contains executable instructions.
	const EXEC_INSTR = 4;
	/// All bits in this mask are reserved for OS-specific
	/// semantics.
	const MASK_OS = 0x0F00_0000;
	/// All bits in this mask are reserved for processor-specific
	/// semantics.
	const MASK_PROC = 0xF000_0000;
    }
}

/// What type of segment is this?
#[derive(Copy, Clone, Debug, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u32)]
pub enum SegmentType {
    /// Undefined.
    Null = 0,
    /// Loadable segment, mapped into memory.
    Load = 1,
    /// Dynamic linking information.
    Dynamic = 2,
    /// Specifies the location and size to a null-terminated path of a
    /// dynamic interpreter.
    Interp = 3,
    /// Specifies the location of notes.
    Note = 4,
    /// Reserved.
    Shlib = 5,
    /// Specifies the location and size of the program header table.
    Phdr = 6,
    /// "Last PT_GNU_STACK program header defines userspace stack
    /// executability (since Linux 2.6.6). Other PT_GNU_STACK headers
    /// are ignored." [1] We need to parse this so it doesn't crash,
    /// but otherwise we don't worry about it!
    ///
    /// [1]: https://docs.kernel.org/next/userspace-api/ELF.html
    GNUStack = 0x6474_E551,
    /// Attributes of RISC-V ELF files
    RVAttributes = 0x7000_0003,
}

#[rustfmt::skip]
bitflags! {
    /// Flags for segments.
    #[repr(C)]
    pub struct SegmentFlags: u32 {
	/// Executable
	const EXEC = 1;
	/// Writable
	const WRITE = 2;
	/// Readable
	const READ = 4;
	/// All bits in this mask are reserved for OS-specific
	/// semantics.
	const MASK_OS = 0x00FF_0000;
	/// All bits in this mask are reserved for processor-specific
	/// semantics.
	const MASK_PROC = 0xFF00_0000;
    }
}

/// An ELF segment, representing a runtime memory region.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// What type of segment is this?
    pub seg_type: SegmentType,
    /// Flags related to segments, typically a subset of the typical
    /// RWX.
    pub flags: SegmentFlags,
    /// Offset of this segment in the file.
    pub offset: usize,
    /// Virtual address of this segment.
    pub virt_addr: usize,
    /// Size of this segment in the file.
    pub file_size: usize,
    /// Size of this segment at runtime. The difference, if any,
    /// between `mem_size` and [`Self::file_size`] is filled with
    /// zeroes.
    pub mem_size: usize,
    /// Alignment of this segment. Zero and one are both equivalent to
    /// no alignment constraints.
    pub align: usize,
}

impl Segment {
    /// Parse a segment.
    ///
    /// # Errors
    ///
    /// This function will return an error if any read or seek
    /// operation failed, or if the segment headere was invalid.
    pub fn parse<R: Read + Seek>(reader: &mut R) -> Result<Self, ParseError> {
        let mut seg = Segment {
            seg_type: SegmentType::Null,
            flags: SegmentFlags::empty(),
            offset: 0,
            virt_addr: 0,
            file_size: 0,
            mem_size: 0,
            align: 0,
        };

        let mut buf = [0; mem::size_of::<raw::ProgramHdr>()];
        reader.read_exact(&mut buf).map_err(|_| ParseError)?;

        // SAFETY: `ProgramHdr` is valid in any bit pattern, and has
        // the same size.
        let prog_hdr = unsafe { mem::transmute::<_, raw::ProgramHdr>(buf) };
        seg.seg_type = SegmentType::try_from(prog_hdr.seg_type).map_err(|_| ParseError)?;
        seg.flags = SegmentFlags::from_bits_truncate(prog_hdr.flags);
        seg.offset = prog_hdr.offset as usize;
        seg.virt_addr = prog_hdr.virt_addr as usize;
        // phys addr is ignored
        seg.file_size = prog_hdr.file_size as usize;
        seg.mem_size = prog_hdr.mem_size as usize;
        seg.align = prog_hdr.align as usize;

        if !prog_hdr.align.is_power_of_two()
            && (prog_hdr.align != 0
                || (seg.seg_type != SegmentType::Null && seg.seg_type != SegmentType::GNUStack))
        {
            return Err(ParseError);
        }

        if seg.align != 0 && seg.virt_addr % seg.align != seg.offset % seg.align {
            return Err(ParseError);
        }

        Ok(seg)
    }
}
