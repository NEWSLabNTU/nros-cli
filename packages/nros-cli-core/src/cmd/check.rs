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
        if let Some(warning) = pending_routing_warning(&cfg) {
            eprintln!("nros check: warning: {warning}");
        }
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

/// Warn when a root `nros.toml` declares `[[bridge]]` groups: those parse +
/// validate, but per-node *bridge* routing isn't emitted yet (bridge nodes bind
/// to the primary session). `[[domain]]` multi-domain routing **is** emitted as
/// of Phase 172.K.5 (`nros deploy` stamps node domains → the generator opens a
/// session per domain + routes via `NodeBuilder::session_idx`), so it no longer
/// warns. `None` ⇒ no bridge config ⇒ no warning. Returned (not printed) so it
/// stays unit-testable.
fn pending_routing_warning(cfg: &WorkspaceConfig) -> Option<String> {
    let all_systems = || cfg.system.iter().chain(cfg.systems.values());
    let bridges: usize = all_systems().map(|s| s.bridge.len()).sum();
    (bridges > 0).then(|| {
        format!(
            "{bridges} [[bridge]] group(s) declared, but per-node bridge routing is not yet \
             emitted — bridge nodes bind to the primary session. (Multi-domain `[[domain]]` \
             routing landed in Phase 172.K.5.)"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_routing_warns_on_bridge_only() {
        let plain: WorkspaceConfig =
            toml::from_str("[workspace]\n[system]\nrmw = \"zenoh\"\n").unwrap();
        assert!(pending_routing_warning(&plain).is_none());

        // [[domain]] is routed now (Phase 172.K.5) → no warning.
        let domained: WorkspaceConfig = toml::from_str(
            "[workspace]\n[system]\nrmw = \"zenoh\"\n\
             [[system.domain]]\nid = 5\nnodes = [\"/talker\"]\n",
        )
        .unwrap();
        assert!(
            pending_routing_warning(&domained).is_none(),
            "[[domain]] routing landed → no warning"
        );

        // [[bridge]] still lacks per-node routing → warns.
        let bridged: WorkspaceConfig = toml::from_str(
            "[workspace]\n[system]\nrmw = \"zenoh\"\n\
             [[system.bridge]]\nname = \"gw\"\n\
             connect = [{ rmw = \"zenoh\", domain = 0 }, { rmw = \"cyclonedds\", domain = 0 }]\n",
        )
        .unwrap();
        let msg = pending_routing_warning(&bridged).expect("bridge ⇒ warning");
        assert!(msg.contains("1 [[bridge]]") && msg.contains("bridge routing is not yet emitted"));
    }
}
