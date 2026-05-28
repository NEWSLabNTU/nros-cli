//! `nros doctor` — Phase 111.A.7. Aggregates per-platform doctors.
//!
//! v1 strategy: shell out to `just doctor` from the detected workspace
//! root. The justfile already orchestrates every per-module doctor
//! recipe (`just nuttx doctor`, `just zephyr doctor`, ...) and is the
//! source of truth for what "healthy" means. We surface the existing
//! mechanism through a single user-facing verb instead of recreating
//! the diagnostic surface from scratch.

use clap::Args as ClapArgs;
use eyre::{Result, WrapErr, bail, eyre};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::{
    cmd::board::find_workspace_root,
    orchestration::{
        root_config::{VendorDir, WorkspaceConfig},
        sdk_index::SdkIndex,
    },
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Restrict the check to one module (e.g. `nuttx`, `zephyr`,
    /// `freertos`). Forwarded as `just <platform> doctor`.
    #[arg(long)]
    pub platform: Option<String>,

    /// Path to the nano-ros workspace root (auto-detected if omitted)
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Root nros.toml whose deploy-target vendor pins to check (Phase 172 WP-A)
    #[arg(long, default_value = "nros.toml")]
    pub config: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    // Phase 172 WP-A — deploy vendor-pin drift check. Engages when the given
    // config is a loadable workspace-root nros.toml; reports each deploy
    // target's pinned vendor dir. `None` ⇒ no root config here (e.g. running
    // in the nano-ros repo) → only the workspace health below runs.
    let pin_problems = check_deploy_pins(&args.config)?;

    // Phase 187.7 — license-gated SDK presence (NVIDIA SPE, ARM FVP, …): never
    // fetched, only instructed. Read before `args.workspace` is moved below.
    let gate_problems = check_license_gates(args.workspace.as_deref())?;

    // The nano-ros workspace health (`just doctor`). When a root nros.toml was
    // checked, missing it is non-fatal (we're in a user deploy project, not the
    // nano-ros repo); otherwise it stays a hard requirement.
    let root = match args.workspace {
        Some(p) => Some(p),
        None => match find_workspace_root() {
            Ok(r) => Some(r),
            Err(_) if pin_problems.is_some() => {
                eprintln!(
                    "nros doctor: no nano-ros workspace here — skipped `just doctor` \
                     (checked deploy pins only)"
                );
                None
            }
            Err(e) => {
                return Err(e).wrap_err(
                    "could not auto-detect the nano-ros workspace root; \
                     pass --workspace <path> explicitly",
                );
            }
        },
    };

    if let Some(root) = root {
        run_just_doctor(&root, args.platform.as_deref())?;
    }

    let problems = pin_problems.unwrap_or(0) + gate_problems;
    if problems > 0 {
        bail!("nros doctor: {problems} problem(s) (deploy pins + license gates)");
    }
    Ok(())
}

/// Phase 187.7 — license-gate presence check. For each `[gated.*]` SDK in the
/// index (NVIDIA SPE, ARM FVP, …), report whether its env var resolves to an
/// existing directory. These are NEVER fetched or built — only instructed. An
/// unset env is informational (the user simply isn't targeting that board); an
/// env that's set but points nowhere is a misconfiguration (counted). No index
/// nearby ⇒ skip silently.
fn check_license_gates(workspace: Option<&Path>) -> Result<usize> {
    let Some(index_path) = crate::cmd::setup::locate_index(workspace) else {
        return Ok(0);
    };
    let index = SdkIndex::load(&index_path)?;
    if index.gated.is_empty() {
        return Ok(0);
    }

    eprintln!("nros doctor: license-gated SDKs ({})", index_path.display());
    let mut problems = 0usize;
    for (name, g) in &index.gated {
        let via = g
            .installer
            .as_deref()
            .map(|i| format!(", via {i}"))
            .unwrap_or_default();
        match std::env::var_os(&g.env) {
            None => eprintln!(
                "  [--] {name} {}: not installed — set ${}{via} (never auto-fetched)",
                g.version, g.env
            ),
            Some(v) => {
                let dir = PathBuf::from(&v);
                if dir.is_dir() {
                    eprintln!("  [OK] {name} {}: ${} = {}", g.version, g.env, dir.display());
                } else {
                    eprintln!(
                        "  [!!] {name}: ${} set to {} — not a directory",
                        g.env,
                        dir.display()
                    );
                    problems += 1;
                }
            }
        }
    }
    Ok(problems)
}

/// Report each deploy target's vendor-pin status. Returns the problem count,
/// or `None` when `config` is not a loadable workspace-root nros.toml.
fn check_deploy_pins(config: &Path) -> Result<Option<usize>> {
    if !config.is_file() {
        return Ok(None);
    }
    // A component nros.toml (not a workspace root) fails to load — skip silently.
    let Ok(cfg) = WorkspaceConfig::load(config) else {
        return Ok(None);
    };
    let root = config
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    eprintln!("nros doctor: deploy targets ({})", config.display());
    let mut problems = 0usize;
    for (name, deploy) in &cfg.deploy {
        let Some(vendor) = &deploy.vendor else {
            eprintln!("  [--] {name}: no vendor pin");
            continue;
        };
        let Some(pin) = &vendor.pin else {
            eprintln!("  [--] {name}: vendor, no pin");
            continue;
        };
        match resolve_vendor_dir(&root, &vendor.dir) {
            Some(dir) if dir.exists() => {
                eprintln!("  [OK] {name}: pinned '{pin}' at {}", dir.display());
            }
            Some(dir) => {
                eprintln!(
                    "  [!!] {name}: pinned '{pin}' — dir {} not found",
                    dir.display()
                );
                problems += 1;
            }
            None => {
                eprintln!("  [!!] {name}: pinned '{pin}' — dir unset (set the env or a default)");
                problems += 1;
            }
        }
    }
    Ok(Some(problems))
}

fn resolve_vendor_dir(root: &Path, dir: &VendorDir) -> Option<PathBuf> {
    dir.resolve()
        .map(|d| if d.is_absolute() { d } else { root.join(d) })
}

fn run_just_doctor(root: &Path, platform: Option<&str>) -> Result<()> {
    if which("just").is_err() {
        return Err(eyre!(
            "`just` is not on PATH. Install it (https://just.systems) \
             or run individual checks manually."
        ));
    }

    let mut cmd = Command::new("just");
    cmd.current_dir(root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match platform {
        Some(p) => {
            cmd.arg(p).arg("doctor");
        }
        None => {
            cmd.arg("doctor");
        }
    }

    let status = cmd
        .status()
        .wrap_err_with(|| format!("failed to invoke `just` in {}", root.display()))?;
    if !status.success() {
        return Err(eyre!(
            "doctor reported failures (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

fn which(bin: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").ok_or_else(|| eyre!("PATH unset"))?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(eyre!("{bin} not found on PATH"))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_gate_flags_misconfigured_env_only() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = std::env::temp_dir().join(format!("nros_gate_{n}"));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("nros-sdk-index.toml"),
            "[gated.nv-spe-fsp]\nversion=\"36.3\"\nenv=\"NROS_TEST_GATE_ENV\"\ninstaller=\"x\"\n",
        )
        .unwrap();
        let env = "NROS_TEST_GATE_ENV";

        // Unset ⇒ informational, not a problem.
        unsafe { std::env::remove_var(env) };
        assert_eq!(check_license_gates(Some(&ws)).unwrap(), 0);
        // Set to a non-existent dir ⇒ misconfigured ⇒ 1 problem.
        unsafe { std::env::set_var(env, ws.join("nope")) };
        assert_eq!(check_license_gates(Some(&ws)).unwrap(), 1);
        // Set to an existing dir ⇒ OK.
        unsafe { std::env::set_var(env, &ws) };
        assert_eq!(check_license_gates(Some(&ws)).unwrap(), 0);

        unsafe { std::env::remove_var(env) };
        std::fs::remove_dir_all(&ws).ok();
    }
}
