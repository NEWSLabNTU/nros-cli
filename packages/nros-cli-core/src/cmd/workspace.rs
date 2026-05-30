//! `nros workspace …` — workspace-level msg-pkg surface.
//!
//! Phase 210.B.3: `nros workspace env [<dir>]` prints the shell `export`
//! line that points `NROS_INTERFACE_SEARCH_PATH` at a workspace's `src/`
//! root (mirrors colcon's `source install/setup.bash` ergonomics). The
//! cmake-side smart Find-stub (`_NrosFindRosMsgPackage.cmake`) +
//! `nros_workspace_interfaces()` both honour this env var.
//!
//! Usage:
//!
//!   eval "$(nros workspace env)"            # uses ./src
//!   eval "$(nros workspace env ./src)"      # explicit path
//!   eval "$(nros workspace env /abs/path)"  # absolute
//!
//! The output prepends to the existing `$NROS_INTERFACE_SEARCH_PATH` so
//! sourcing multiple workspaces stacks correctly. Fish shell users:
//!
//!   nros workspace env --shell fish | source

use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use eyre::{Result, eyre};
use std::path::PathBuf;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Sub,
}

#[derive(Debug, Subcommand)]
pub enum Sub {
    /// Print shell export adding <dir> (default `./src`) to
    /// `NROS_INTERFACE_SEARCH_PATH`. `eval "$(nros workspace env)"`.
    Env(EnvArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Shell {
    /// POSIX-shell `export VAR=…` (bash/zsh/sh).
    Posix,
    /// Fish-shell `set -gx VAR …`.
    Fish,
}

#[derive(Debug, ClapArgs)]
pub struct EnvArgs {
    /// Workspace root containing pkg subdirs with `package.xml`. Defaults
    /// to `./src` (the colcon-standard layout).
    pub workspace: Option<PathBuf>,

    /// Output shell flavour.
    #[arg(long, value_enum, default_value = "posix")]
    pub shell: Shell,
}

pub fn run(args: Args) -> Result<()> {
    match args.command {
        Sub::Env(env_args) => run_env(env_args),
    }
}

fn run_env(args: EnvArgs) -> Result<()> {
    let ws = args.workspace.unwrap_or_else(|| PathBuf::from("./src"));
    let abs = std::fs::canonicalize(&ws)
        .map_err(|e| eyre!("workspace env: {}: {e}", ws.display()))?;
    let abs_s = abs.display().to_string();

    // Print the export line.  Reading the EXISTING env var here would
    // resolve at `nros` invocation time (which doesn't see the calling
    // shell's already-prepended entries when chained `eval` calls stack);
    // print the variable substitution literally so the shell expands it
    // at eval time and stacking works.
    match args.shell {
        Shell::Posix => {
            println!("export NROS_INTERFACE_SEARCH_PATH=\"{abs_s}:${{NROS_INTERFACE_SEARCH_PATH:-}}\"");
        }
        Shell::Fish => {
            println!("set -gx NROS_INTERFACE_SEARCH_PATH \"{abs_s}\" $NROS_INTERFACE_SEARCH_PATH");
        }
    }
    Ok(())
}
