#![feature(slice_flatten)]
use std::{env, mem, path::PathBuf, process::Command, slice};

use bootfs::{BootFsEntry, BootFsHeader};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/linker.ld");

    let procsvr = PathBuf::from(env::var("CARGO_BIN_FILE_PROCSVR").unwrap());
    println!("cargo:rerun-if-changed={}", procsvr.display());
    let procsvr_bin = procsvr.with_file_name("procsvr.bin");
    println!("{}", procsvr_bin.display());

    Command::new("rust-objcopy")
        .args([
            "-O",
            "binary",
            procsvr.to_str().unwrap(),
            procsvr_bin.to_str().unwrap(),
            "--remove-section=.stack",
            "--keep-section=.sbss",
            "--keep-section=.bss",
        ])
        .spawn()
        .unwrap()
        .wait()
        .unwrap();

    let files = vec![(
        String::from("procsvr"),
        std::fs::read(&procsvr_bin).unwrap(),
    )];
    println!("{:#?}", files);

    let mut bootfs = PathBuf::from(env::var("OUT_DIR").unwrap());
    bootfs.push("bootfs.lz4");

    std::fs::write(&bootfs, make_bootfs(files)).unwrap();

    println!(
        "cargo:rustc-env=CARGO_BUILD_BOOTFS_PATH={}",
        bootfs.display()
    );

    println!(
        "cargo:rustc-link-arg=-T{}/src/linker.ld",
        env::var("CARGO_MANIFEST_DIR").unwrap()
    );
    println!("cargo:rustc-link-arg=-melf64lriscv");
    println!("cargo:rustc-link-arg=-nostdlib");
}

fn make_bootfs(files: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let header = BootFsHeader {
        magic: 0x0B007F50,
        n_entries: files.len() as u32,
    };
    let mut entries = vec![];
    let mut body = vec![];
    let base_offset = mem::size_of::<BootFsHeader>() + files.len() * mem::size_of::<BootFsEntry>();
    for (name, file) in files {
        let offset = body.len();
        let length = file.len();
        body.extend(file);
        let name_offset = body.len();
        body.extend(name.as_bytes());
        entries.push(BootFsEntry {
            name_offset: base_offset as u32 + name_offset as u32,
            name_length: name.len() as u32,
            offset: base_offset as u32 + offset as u32,
            length: length as u32,
        });
    }

    let header: &[u8] =
        &unsafe { mem::transmute::<BootFsHeader, [u8; mem::size_of::<BootFsHeader>()]>(header) };
    let entries = unsafe {
        slice::from_raw_parts(
            entries
                .as_ptr()
                .cast::<[u8; mem::size_of::<BootFsEntry>()]>(),
            entries.len(),
        )
    }
    .flatten();

    let mut v = Vec::new();
    v.extend(header);
    v.extend(entries);
    v.extend(body);

    lz4_flex::compress_prepend_size(&v)
}
