use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/linker.ld");

    let mut init = PathBuf::from(env::var("OUT_DIR").unwrap());
    init.pop();
    init.pop();
    init.pop();
    init.push("userspace_testing");
    println!("cargo:rerun-if-changed={}", init.display());
    let init_bin = init.with_file_name("init.bin");

    Command::new("rust-objcopy")
        .args([
            "-O",
            "binary",
            init.to_str().unwrap(),
            init_bin.to_str().unwrap(),
            "--remove-section=.stack",
            "--keep-section=.sbss",
            "--keep-section=.bss",
        ])
        .spawn()
        .unwrap();

    println!(
        "cargo:rustc-env=CARGO_BUILD_INIT_PATH={}",
        init_bin.display()
    );

    println!(
        "cargo:rustc-link-arg=-T{}/src/linker.ld",
        env::var("CARGO_MANIFEST_DIR").unwrap()
    );
    println!("cargo:rustc-link-arg=-melf64lriscv");
    println!("cargo:rustc-link-arg=-nostdlib");
}
