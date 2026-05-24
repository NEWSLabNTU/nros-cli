//! `nros new <name>` — Phase 111.A.4.
//!
//! Forwards to `cargo_nano_ros::scaffold::scaffold_package` so the CLI
//! stays in lockstep with the shared scaffolding implementation.
//! Use-case (`talker` / `listener` / `service` / `action`) and RMW-choice
//! diversification are accepted at the CLI for forward-compat but
//! currently affect only the printed "Next steps" banner — full
//! per-use-case template trees land alongside the Phase 112 example
//! sweep.

use cargo_nano_ros::scaffold::{ScaffoldConfig, scaffold_package};
use clap::Args as ClapArgs;
use eyre::Result;
use std::path::PathBuf;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Project directory to create
    pub name: PathBuf,

    /// Target platform
    #[arg(long, value_parser = ["native", "freertos", "nuttx", "threadx", "zephyr", "esp32", "posix", "baremetal"])]
    pub platform: String,

    /// RMW backend
    #[arg(long, value_parser = ["zenoh", "xrce", "cyclonedds"], default_value = "zenoh")]
    pub rmw: String,

    /// Source language
    #[arg(long, value_parser = ["rust", "c", "cpp"], default_value = "rust")]
    pub lang: String,

    /// Use case template
    #[arg(long = "use-case", value_parser = ["talker", "listener", "service", "action"], default_value = "talker")]
    pub use_case: String,

    /// Overwrite an existing directory
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: Args) -> Result<()> {
    let name = args
        .name
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| eyre::eyre!("invalid project name: {}", args.name.display()))?
        .to_string();
    scaffold_package(&ScaffoldConfig {
        name,
        lang: args.lang,
        platform: args.platform,
        rmw: args.rmw,
        use_case: args.use_case,
        force: args.force,
    })
}
