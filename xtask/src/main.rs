use clap::Parser;
use color_eyre::eyre;

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "snake_case")]
enum Arguments {
    /// Compile all packages
    Build(BuildOptions),
    /// Check if all packages compile, but don't build artifacts
    Check(BuildOptions),
    /// Run clippy on all packages
    Clippy(BuildOptions),
    /// Build documentation for all packages
    Doc(DocOptions),
    /// Clean up build artifacts
    Clean,
    /// Run under QEMU
    Run(RunOptions),
}

#[derive(Clone, Debug, Parser)]
struct DocOptions {
    #[clap(flatten)]
    inner: BuildOptions,

    /// Open docs in browser when done building
    #[clap(long)]
    open: bool,

    /// Show doc coverage (note: this does not build HTML docs!)
    #[clap(long)]
    show_coverage: bool,

    /// Document private items
    #[clap(long)]
    private: bool,
}

mod build;

use build::{BuildCtx, BuildMode, BuildOptions, RunOptions};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    let args = Arguments::parse();

    let mut ctx = BuildCtx::new()?;

    match args {
        Arguments::Build(opts) => ctx.build(opts, BuildMode::Build, true)?,
        Arguments::Check(opts) => ctx.build(opts, BuildMode::Check, true)?,
        Arguments::Clippy(opts) => ctx.build(opts, BuildMode::Clippy, true)?,
        Arguments::Clean => ctx.clean()?,
        Arguments::Doc(opts) => ctx.build(
            opts.inner,
            BuildMode::Doc {
                open: opts.open,
                coverage: opts.show_coverage,
                private: opts.private,
            },
            true,
        )?,
        Arguments::Run(opts) => ctx.run(opts).await?,
    }

    Ok(())
}
