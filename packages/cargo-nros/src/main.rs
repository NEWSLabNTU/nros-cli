//! `cargo-nros` — cargo subcommand front-end for the nros CLI.
//!
//! Cargo auto-discovers `cargo-<verb>` binaries on PATH and invokes them as
//! `cargo-<verb> <verb> <args...>`. This shell:
//!   1. Strips the cargo-injected `nros` at argv[1] (when present).
//!   2. Intercepts the mandatory `--explain` flag (decomposes to underlying
//!      `nros …` invocation without running it). Phase 212.A.2.
//!   3. Re-exports `nros_cli_core::run` for every other verb. Phase 212.A.1.
//!
//! Hard cap: ≤100 LoC (verified by `tokei src/`).

use clap::{CommandFactory, Parser};
use eyre::Result;
use nros_cli_core::cmd::Cmd;

#[derive(Parser, Debug)]
#[command(
    name = "cargo-nros",
    bin_name = "cargo nros",
    about = "Run any nros verb as a cargo subcommand: `cargo nros <verb> …`.",
    long_about = "cargo-nros — cargo subcommand front-end for the nros CLI.\n\n\
                  Every `nros <verb>` works as `cargo nros <verb>`. Pass \
                  --explain to print the underlying `nros` invocation without \
                  running it (Phase 212.A.2).",
    version,
    propagate_version = true
)]
struct Cli {
    /// Print the underlying `nros …` invocation and exit without running it.
    #[arg(long, global = true)]
    explain: bool,

    #[command(subcommand)]
    command: Cmd,
}

/// Normalise the raw argv vector cargo hands us.
///
/// Cargo invokes `cargo-nros nros <args…>` for the `cargo nros` subcommand.
/// Strip the injected `nros` at position 1 so clap sees a clean argv. If the
/// user invokes the binary directly (no `nros` at position 1), leave it alone.
fn strip_cargo_subcommand(mut argv: Vec<String>) -> Vec<String> {
    if argv.len() >= 2 && argv[1] == "nros" {
        argv.remove(1);
    }
    argv
}

/// Render a `cargo nros …` invocation back to its `nros …` shape for
/// `--explain`. We deliberately re-stringify rather than reach into clap's
/// matches because every verb (current + future) is covered for free.
fn explain_invocation(argv: &[String]) -> String {
    let rest: Vec<&str> = argv
        .iter()
        .skip(1)
        .filter(|a| a.as_str() != "--explain")
        .map(String::as_str)
        .collect();
    format!("Would run: nros {}", rest.join(" "))
}

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    let normalised = strip_cargo_subcommand(argv);

    // Intercept --explain before clap dispatch so verbs that take their own
    // subcommands (e.g. `config`, `board`) don't need a per-verb flag wired.
    if normalised.iter().any(|a| a == "--explain") {
        println!("{}", explain_invocation(&normalised));
        return Ok(());
    }

    // Special-case --help / -h at the cargo-nros layer so we render our own
    // bin_name. Sub-verb help still flows through clap normally.
    if normalised.len() == 1 {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    }

    let cli = match Cli::try_parse_from(normalised) {
        Ok(c) => c,
        Err(e) => e.exit(), // clap's exit() respects --help / --version cleanly
    };
    nros_cli_core::run(cli.command)
}
