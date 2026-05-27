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
use eyre::{Result, bail};
use std::path::PathBuf;

use crate::{
    cmd::scaffold_deploy::{DeployScaffold, scaffold_deploy},
    orchestration::root_config::DeployKind,
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Project directory to create (project mode)
    pub name: Option<PathBuf>,

    /// Target platform (required in project mode)
    #[arg(long, value_parser = ["native", "freertos", "nuttx", "threadx", "zephyr", "esp32", "posix", "baremetal"])]
    pub platform: Option<String>,

    /// RMW backend
    #[arg(long, value_parser = ["zenoh", "xrce", "cyclonedds"], default_value = "zenoh")]
    pub rmw: String,

    /// Source language
    #[arg(long, value_parser = ["rust", "c", "cpp"], default_value = "rust")]
    pub lang: String,

    /// Use case template
    #[arg(long = "use-case", value_parser = ["talker", "listener", "service", "action"], default_value = "talker")]
    pub use_case: String,

    /// Phase 172 WP-A — scaffold a `[deploy.<name>]` target in the root
    /// nros.toml (deploy mode) instead of a project.
    #[arg(long)]
    pub deploy: Option<String>,

    /// Deploy kind (deploy mode)
    #[arg(long, value_parser = ["self", "vendor-lib", "vendor-module"], default_value = "self")]
    pub kind: String,

    /// Cargo target triple (deploy mode)
    #[arg(long)]
    pub target: Option<String>,

    /// Board (deploy mode, vendor-module)
    #[arg(long)]
    pub board: Option<String>,

    /// Overwrite an existing directory / `[deploy.<name>]` table
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: Args) -> Result<()> {
    // Deploy mode (Phase 172 WP-A): `nros new --deploy <name> --kind <k> ...`.
    if let Some(deploy_name) = args.deploy {
        let kind = match args.kind.as_str() {
            "self" => DeployKind::Self_,
            "vendor-lib" => DeployKind::VendorLib,
            "vendor-module" => DeployKind::VendorModule,
            other => bail!("unknown deploy kind: {other}"),
        };
        return scaffold_deploy(&DeployScaffold {
            name: deploy_name,
            kind,
            target: args.target,
            board: args.board,
            root: std::env::current_dir()?,
            force: args.force,
        });
    }

    // Project mode.
    let name = args
        .name
        .as_ref()
        .ok_or_else(|| eyre::eyre!("`nros new <name>` requires a project name"))?
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| eyre::eyre!("invalid project name"))?
        .to_string();
    let platform = args
        .platform
        .ok_or_else(|| eyre::eyre!("`nros new <name>` requires `--platform <p>`"))?;
    scaffold_package(&ScaffoldConfig {
        name,
        lang: args.lang,
        platform,
        rmw: args.rmw,
        use_case: args.use_case,
        force: args.force,
    })
}
