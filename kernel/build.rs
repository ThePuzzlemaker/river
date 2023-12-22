use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/linker.ld");

    let init = PathBuf::from(env::var("CARGO_BIN_FILE_INIT").unwrap());
    println!("cargo:rerun-if-changed={}", init.display());
    let init_bin = init.with_file_name("init.bin");

    if env::var("PROFILE").unwrap() == "release" {
        Command::new("rust-strip")
            .args(["-s", init.to_str().unwrap()])
            .spawn()
            .unwrap()
            .wait()
            .unwrap();
    }

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
        .unwrap()
        .wait()
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
