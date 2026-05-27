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
    orchestration::root_config::{VendorDir, WorkspaceConfig},
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

    if let Some(n) = pin_problems
        && n > 0
    {
        bail!("nros doctor: {n} deploy vendor-pin problem(s)");
    }
    Ok(())
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
