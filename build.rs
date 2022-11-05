fn main() {
    println!("cargo:rerun-if-changed=src/entry.S");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/linker.ld");
    println!("cargo:rustc-link-arg=-Tsrc/linker.ld");
    println!("cargo:rustc-link-arg=-melf64lriscv");
    println!("cargo:rustc-link-arg=-nostdlib");
    if std::env::var("CROSS_COMPILE").is_err() {
        // for my IDE. change this as needed.
        std::env::set_var("CROSS_COMPILE", "riscv64-elf");
    }
    // cc::Build::new()
    //     .flag("-mcmodel=medany")
    //     .flag("-mabi=lp64d")
    //     .flag("-march=rv64gc")
    //     .flag("-fno-stack-protector")
    //     .flag("-ffreestanding")
    //     .warnings(true)
    //     .warnings_into_errors(true)
    //     .file("src/entry.S")
    //     .file("src/trap.S")
    //     .compile("riverasm");
}
