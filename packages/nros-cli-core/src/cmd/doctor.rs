//! `nros doctor` — Phase 111.A.7. Aggregates per-platform doctors.
//!
//! v1 strategy: shell out to `just doctor` from the detected workspace
//! root. The justfile already orchestrates every per-module doctor
//! recipe (`just nuttx doctor`, `just zephyr doctor`, ...) and is the
//! source of truth for what "healthy" means. We surface the existing
//! mechanism through a single user-facing verb instead of recreating
//! the diagnostic surface from scratch.

use clap::Args as ClapArgs;
use eyre::{Result, WrapErr, eyre};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::cmd::board::find_workspace_root;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Restrict the check to one module (e.g. `nuttx`, `zephyr`,
    /// `freertos`). Forwarded as `just <platform> doctor`.
    #[arg(long)]
    pub platform: Option<String>,

    /// Path to the nano-ros workspace root (auto-detected if omitted)
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let root = match args.workspace {
        Some(p) => p,
        None => find_workspace_root().wrap_err(
            "could not auto-detect the nano-ros workspace root; \
             pass --workspace <path> explicitly",
        )?,
    };

    if which("just").is_err() {
        return Err(eyre!(
            "`just` is not on PATH. Install it (https://just.systems) \
             or run individual checks manually."
        ));
    }

    let mut cmd = Command::new("just");
    cmd.current_dir(&root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match &args.platform {
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
