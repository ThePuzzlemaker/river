use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/linker.ld");

    let procsvr = PathBuf::from(env::var("CARGO_BIN_FILE_PROCSVR").unwrap());
    println!("cargo:rerun-if-changed={}", procsvr.display());
    let procsvr_bin = procsvr.with_file_name("procsvr.bin");

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
        .unwrap();

    println!(
        "cargo:rustc-env=CARGO_BUILD_PROCSVR_PATH={}",
        procsvr_bin.display()
    );

    println!(
        "cargo:rustc-link-arg=-T{}/src/linker.ld",
        env::var("CARGO_MANIFEST_DIR").unwrap()
    );
    println!("cargo:rustc-link-arg=-melf64lriscv");
    println!("cargo:rustc-link-arg=-nostdlib");
}
