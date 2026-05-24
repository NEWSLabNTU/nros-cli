//! The `nros` standalone binary — Phase 111.A.2.
//!
//! Pure clap dispatch shell. All real work lives in `nros-cli-core`.

use clap::{CommandFactory, Parser};
use clap_complete::{Shell, generate};
use eyre::Result;
use nros_cli_core::cmd::Cmd;
use std::io;

#[derive(Parser, Debug)]
#[command(
    name = "nros",
    about = "nano-ros user CLI",
    version,
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // `completions` is wired here (not in nros-cli-core) because clap_complete
    // needs the binary's `clap::Command` tree, which lives at the
    // front-end. Phase 111.A.13.
    if let Cmd::Completions(args) = &cli.command {
        let shell: Shell = args
            .shell
            .parse()
            .map_err(|e| eyre::eyre!("unsupported shell `{}`: {e}", args.shell))?;
        let mut cmd = Cli::command();
        let bin = cmd.get_name().to_string();
        generate(shell, &mut cmd, bin, &mut io::stdout());
        return Ok(());
    }
    nros_cli_core::run(cli.command)
}
