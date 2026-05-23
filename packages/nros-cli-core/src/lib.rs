//! Shared library backing the `nros` CLI.
//!
//! `nros-cli` owns the canonical user-facing command surface. The older
//! `cargo-nano-ros` subcommand remains as a compatibility adapter for
//! Cargo workflows, while new scripts should call `nros` directly.

pub mod cmd;
pub mod orchestration;

use eyre::Result;

/// Top-level dispatcher entry point — every binary front-end lands here.
///
/// `argv` is the post-clap parsed command structure. Each variant maps
/// 1:1 to a `nros <verb>` invocation.
pub fn run(cmd: cmd::Cmd) -> Result<()> {
    match cmd {
        cmd::Cmd::New(args) => cmd::new::run(args),
        cmd::Cmd::Generate(args) => cmd::generate::run(args),
        cmd::Cmd::GenerateRust(args) => cmd::generate::run_rust(args),
        cmd::Cmd::Metadata(args) => cmd::metadata::run(args),
        cmd::Cmd::Plan(args) => cmd::plan::run(args),
        cmd::Cmd::Check(args) => cmd::check::run(args),
        cmd::Cmd::Config(args) => cmd::config::run(args),
        cmd::Cmd::Build(args) => cmd::build::run(args),
        cmd::Cmd::Run(args) => cmd::run_target::run(args),
        cmd::Cmd::Monitor(args) => cmd::monitor::run(args),
        cmd::Cmd::Doctor(args) => cmd::doctor::run(args),
        cmd::Cmd::Board(args) => cmd::board::run(args),
        cmd::Cmd::Version => cmd::version::run(),
        cmd::Cmd::Completions(args) => cmd::completions::run(args),
        #[cfg(feature = "release")]
        cmd::Cmd::Release(args) => cmd::release::run(args),
    }
}
