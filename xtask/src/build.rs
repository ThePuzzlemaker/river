use clap::Parser;
use color_eyre::eyre;
use tokio::process::Command;
use xshell::cmd;

use std::{env, path::PathBuf, process::Stdio};

pub enum BuildMode {
    Build,
    Check,
    Clippy,
    Doc {
        open: bool,
        coverage: bool,
        private: bool,
    },
}

pub struct BuildCtx {
    shell: xshell::Shell,
    cargo_cmd: String,
}

#[derive(Clone, Debug, Parser)]
pub struct BuildOptions {
    /// Show more information
    #[clap(short, long)]
    verbose: bool,

    /// Build in release mode
    #[clap(long)]
    release: bool,

    /// Extra options to pass to the underlying command
    #[clap(last = true, required = false)]
    extra: Vec<String>,

    /// Build OpenSBI with a non-LLVM compiler
    #[clap(long)]
    no_llvm: bool,
}

impl BuildCtx {
    pub fn new() -> eyre::Result<Self> {
        let cargo_cmd = env::var("CARGO").unwrap_or_else(|_| String::from("cargo"));

        Ok(Self {
            shell: xshell::Shell::new()?,
            cargo_cmd,
        })
    }

    pub fn clean(&mut self) -> eyre::Result<()> {
        for (package, dir) in Self::get_build_dirs()? {
            let cargo = &self.cargo_cmd;
            let _cwd = self.shell.push_dir(dir);

            cmd!(self.shell, "{cargo} clean -p{package}").run()?;
        }

        {
            let _cwd = self.shell.push_dir("opensbi/");

            cmd!(self.shell, "make clean").run()?;
        }
        cmd!(self.shell, "cargo clean").run()?;

        Ok(())
    }

    pub fn get_build_dirs() -> eyre::Result<Vec<(String, PathBuf)>> {
        let mut dirs = vec![(String::from("rille"), PathBuf::from("rille/"))];
        for dir in glob::glob("user/*")? {
            let dir = dir?;
            let basename = dir
                .as_path()
                .components()
                .last()
                .unwrap()
                .as_os_str()
                .to_string_lossy()
                .to_string();
            if basename == "scripts" {
                continue;
            }
            dirs.push((basename, dir));
        }
        dirs.push((String::from("river"), PathBuf::from("kernel/")));
        Ok(dirs)
    }

    pub fn build(
        &mut self,
        opts: super::BuildOptions,
        mode: BuildMode,
        allow_extra: bool,
    ) -> eyre::Result<()> {
        if let BuildMode::Doc {
            open,
            coverage,
            private,
        } = mode
        {
            let profile = if opts.release { "release" } else { "dev" };
            let verbose: &[&str] = if opts.verbose { &["-vv"] } else { &[] };
            let cargo = &self.cargo_cmd;
            let extra = if allow_extra { &*opts.extra } else { &[] };
            let open: &[&str] = if open { &["--open"] } else { &[] };
            let private: &[&str] = if private {
                &["--document-private-items"]
            } else {
                &[]
            };
            if coverage {
                std::env::set_var(
                    "RUSTDOCFLAGS",
                    &format!(
                        "-Zunstable-options --show-coverage {}",
                        std::env::var("RUSTDOCFLAGS").unwrap_or_default()
                    ),
                )
            }

            cmd!(
                self.shell,
                "{cargo} doc --profile {profile}
                {verbose...} {extra...} {private...}
                --target riscv64gc-unknown-none-elf --workspace
                --exclude xtask {open...}"
            )
            .run()?;

            return Ok(());
        }

        //for (_package, dir) in Self::get_build_dirs()? {
        {
            //let _is_last = dir.as_path() == Path::new("kernel/");
            //let _cwd = self.shell.push_dir(dir.clone());
            let _cwd = self.shell.push_dir("kernel/");

            let subcommand = match mode {
                BuildMode::Build => "build",
                BuildMode::Check => "check",
                BuildMode::Clippy => "clippy",
                BuildMode::Doc { .. } => unreachable!(),
            };

            let profile = if opts.release { "release" } else { "dev" };
            let verbose: &[&str] = if opts.verbose { &["-vv"] } else { &[] };
            let cargo = &self.cargo_cmd;
            let extra = if allow_extra { &*opts.extra } else { &[] };
            let mode_extra: Vec<&str> = match mode {
                _ => vec![],
            };

            cmd!(
                self.shell,
                "{cargo} {subcommand} --profile
                {profile} {verbose...} {extra...} {mode_extra...}
                --target riscv64gc-unknown-none-elf --workspace --exclude xtask"
            )
            .run()?;
        }

        {
            let _cwd = self.shell.push_dir("opensbi/");

            let target_profile_path = if opts.release { "release" } else { "debug" };

            let llvm = if opts.no_llvm { "" } else { " LLVM=1" };
            cmd!(
		self.shell,
		"make PLATFORM=generic{llvm} FW_PAYLOAD=../target/riscv64gc-unknown-none-elf/{target_profile_path}/river"
	    ).run()?;
        }

        Ok(())
    }

    pub async fn run(&mut self, opts: RunOptions) -> eyre::Result<()> {
        let target_profile_path = if opts.build_opts.release {
            "release"
        } else {
            "debug"
        };
        let extras = opts.build_opts.extra.clone();

        self.build(opts.build_opts, BuildMode::Build, false)?;

        let memory = opts.memory;

        let debugger_extras: &[&str] = if opts.debugger { &["-s", "-S"] } else { &[] };
        let nographic: &[&str] = if opts.no_graphic {
            &["-nographic", "-monitor", "/dev/null"]
        } else {
            &[]
        };
        let smp: Vec<String> = if let Some(smp) = opts.smp {
            vec!["-smp".to_string(), format!("cores={smp}")]
        } else {
            vec![]
        };
        let dump_traps: &[&str] = if opts.dump_traps { &["-d", "int"] } else { &[] };

        let mut qemu_child = Command::new("qemu-system-riscv64")
            .args([
                "-M",
                "virt",
                "-m",
                &memory,
                "-bios",
                "opensbi/build/platform/generic/firmware/fw_jump.bin",
                "-kernel",
                &format!("target/riscv64gc-unknown-none-elf/{target_profile_path}/river"),
                "-serial",
                "stdio",
            ])
            .args(debugger_extras)
            .args(nographic)
            .args(smp)
            .args(dump_traps)
            .args(extras)
            .stdin(if opts.debugger {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .spawn()?;

        let mut gdb_child = if opts.debugger && !opts.no_run_debugger {
            Some(
                Command::new(opts.debugger_path)
                    .arg(&format!(
                        "target/riscv64gc-unknown-none-elf/{target_profile_path}/river"
                    ))
                    .spawn()?,
            )
        } else {
            None
        };

        #[rustfmt::skip]
        tokio::select! {
            status = qemu_child.wait() => {
		let status = status?;
		let code = status.code().unwrap_or_else(|| if status.success() { 0 } else { 1 });
		println!(
		    "QEMU exited with status: {code}{}.",
		    if opts.debugger && !opts.no_run_debugger { ", now killing GDB" } else { "" }
		);
		if let Some(mut gdb_child) = gdb_child {
		    gdb_child.kill().await?;
		}
            }
	    Some(status) = async {
		if let Some(gdb_child) = gdb_child.as_mut() {
		    Some(gdb_child.wait().await)
		} else {
		    None
		}
	    } => {
		let status = status?;
		let code = status.code().unwrap_or_else(|| if status.success() { 0 } else { 1 });
		println!("GDB exited with status: {code}, now killing QEMU.");
		qemu_child.kill().await?;
	    }
        };

        Ok(())
    }
}

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub struct RunOptions {
    #[clap(flatten)]
    build_opts: BuildOptions,

    /// How much memory to provision for the emulator
    #[clap(short, long, default_value = "256M")]
    memory: String,

    /// How many processors to allocate for the emulator, default is
    /// 1.
    #[clap(long)]
    smp: Option<usize>,

    /// Disable QEMU debug window
    #[clap(long, default_value_t = false)]
    no_graphic: bool,

    /// Enable debugger
    #[clap(short, long, default_value_t = false)]
    debugger: bool,

    #[clap(short('n'), long, default_value_t = false)]
    no_run_debugger: bool,

    /// Set path to debugger
    #[clap(long, default_value = "riscv64-linux-gnu-gdb")]
    debugger_path: String,

    /// Log all traps to the serial console.
    #[clap(long, default_value_t = false)]
    dump_traps: bool,
}
