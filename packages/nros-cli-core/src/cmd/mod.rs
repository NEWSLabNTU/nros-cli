//! Subcommand dispatch surface.
//!
//! Each verb lives in its own submodule and exposes:
//!   * a clap `Args` struct (when the verb takes options)
//!   * a `run(args) -> Result<()>` function
//!
//! `Cmd` is the clap-derived enum the binary front-ends parse into;
//! [`crate::run`] dispatches it.

use clap::Subcommand;

pub mod board;
pub mod bringup;
pub mod build;
pub mod check;
pub mod codegen;
pub mod completions;
pub mod config;
pub mod deploy;
pub mod doctor;
pub mod emit_package_xml;
pub mod explain;
pub mod generate;
pub mod metadata;
pub mod monitor;
pub mod new;
pub mod new_system;
pub mod plan;
pub mod run_target;
pub mod scaffold_deploy;
pub mod setup;
pub mod version;

#[cfg(feature = "release")]
pub mod release;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Scaffold a new nano-ros project (talker / listener / service / action)
    New(new::Args),

    /// Generate Rust / C / C++ message bindings from `package.xml`
    Generate(generate::Args),

    /// Generate Rust message bindings from `package.xml`
    #[command(name = "generate-rust")]
    GenerateRust(generate::RustArgs),

    /// Build-tool C/C++ binding generation (`--args-file` / `resolve-deps`).
    /// The interface the cmake / build.rs consumers speak (Phase 195.A — folds
    /// in the former standalone `nros-codegen` binary).
    Codegen(codegen::Args),

    /// Collect component source metadata for orchestration planning
    Metadata(metadata::Args),

    /// Resolve launch files, manifests, and metadata into nros-plan.json
    Plan(plan::Args),

    /// Validate a generated nros-plan.json
    Check(check::Args),

    /// Render a generated nros-plan.json in human-readable form
    Explain(explain::Args),

    /// Emit a generated artifact for a package (Phase 212.G — `package-xml`).
    #[command(subcommand)]
    Emit(emit_package_xml::What),

    /// Inspect or validate the current project's resolved configuration
    #[command(subcommand)]
    Config(config::Args),

    /// Build the current project (auto-detects cargo / cmake / west)
    Build(build::Args),

    /// Run a [deploy.<name>] target from the root nros.toml (Phase 172 WP-A)
    Deploy(deploy::Args),

    /// Resolve + fetch a board's toolchain/SDK packages (Phase 187)
    Setup(setup::Args),

    /// Build, flash, and monitor the current project on the selected target
    #[command(name = "run")]
    Run(run_target::Args),

    /// Attach to a running target's serial / RTT / semihosting output
    Monitor(monitor::Args),

    /// Health-check the workspace (SDK paths, toolchains, env)
    Doctor(doctor::Args),

    /// Inspect supported boards
    #[command(subcommand)]
    Board(board::Args),

    /// Print toolchain + library versions
    Version,

    /// Generate shell completions (bash | zsh | fish | powershell)
    Completions(completions::Args),

    /// Maintainer-only release subcommands (hidden unless built with
    /// `--features release`)
    #[cfg(feature = "release")]
    #[command(subcommand)]
    Release(release::Args),
}
