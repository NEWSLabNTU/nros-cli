//! `nros check` - validate a generated nros-plan.json or a root nros.toml.

use crate::orchestration::{planner::check_plan_file, root_config::WorkspaceConfig};
use clap::Args as ClapArgs;
use eyre::Result;
use std::path::PathBuf;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to nros-plan.json, or a root nros.toml (Phase 172 WP-A)
    #[arg(default_value = "build/nros/nros-plan.json")]
    pub plan: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    // A `.toml` argument is the workspace-root deployment config; anything
    // else is a generated plan. `WorkspaceConfig::load` validates as it parses.
    if args.plan.extension().is_some_and(|e| e == "toml") {
        let cfg = WorkspaceConfig::load(&args.plan)?;
        let systems = cfg.systems.len() + usize::from(cfg.system.is_some());
        eprintln!(
            "nros check: ok ({} system(s), {} deploy target(s), {})",
            systems,
            cfg.deploy.len(),
            args.plan.display()
        );
        return Ok(());
    }

    let report = check_plan_file(&args.plan)?;
    if report.errors == 0 {
        for message in &report.messages {
            eprintln!("nros check: warning: {message}");
        }
        eprintln!(
            "nros check: ok ({} warning(s), {})",
            report.warnings,
            args.plan.display()
        );
    }
    Ok(())
}

// Phase 172 — `[[bridge]]` per-node routing is now emitted by the generator
// (`register_bridges`: a bridge node per endpoint session + the generic-sub →
// generic-pub relay with `bridge_origin` echo suppression), so the former
// "routing not yet emitted" warning is gone. `[[domain]]` routing landed in
// Phase 172.K.5.
