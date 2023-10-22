use clap::Parser;
use color_eyre::eyre;

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "snake_case")]
enum Arguments {
    Build(BuildOptions),
    Check(BuildOptions),
    Clippy(BuildOptions),
    Doc(DocOptions),
    Clean,
    Run(RunOptions),
}

#[derive(Clone, Debug, Parser)]
struct DocOptions {
    #[clap(flatten)]
    inner: BuildOptions,

    #[clap(long)]
    open: bool,

    #[clap(long)]
    show_coverage: bool,
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
            BuildMode::Doc(opts.open, opts.show_coverage),
            true,
        )?,
        Arguments::Run(opts) => ctx.run(opts).await?,
    }

    Ok(())
}
