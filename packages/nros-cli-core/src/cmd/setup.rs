//! `nros setup` — Phase 187.2: resolve a board's toolchain/SDK package set from
//! the index and report the install plan. The actual fetch / source-build /
//! cache is Phase 187.3; this verb does the CLI + board→package resolution +
//! `--list` / `--licenses` / the per-host disposition plan.
//!
//! See `docs/design/nros-setup-toolchain-management.md`.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use eyre::{Result, WrapErr, bail};

use crate::orchestration::{
    sdk_index::{SdkIndex, host_key},
    sdk_store::{InstallAction, SdkLock, execute, plan_install, store_root, tool_prefix},
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Board to set up (resolves its toolchain/SDK package set).
    pub board: Option<String>,

    /// Resolve by target triple (with or instead of `<board>`).
    #[arg(long)]
    pub target: Option<String>,

    /// List every package in the index + its version.
    #[arg(long)]
    pub list: bool,

    /// Show the license-gated packages + how to install them.
    #[arg(long)]
    pub licenses: bool,

    /// Path to the SDK index.
    #[arg(long, default_value = "nros-sdk-index.toml")]
    pub index: PathBuf,

    /// Resolve + print the plan without fetching/building anything.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let index = SdkIndex::load(&args.index)?;
    let host = host_key();

    if args.list {
        print_list(&index);
        return Ok(());
    }
    if args.licenses {
        print_licenses(&index);
        return Ok(());
    }

    let board = match args.board.as_deref() {
        Some(b) => b,
        None => {
            bail!("nros setup: give a <board> (or `--target <triple>`), `--list`, or `--licenses`")
        }
    };

    let packages = resolve_packages(board, args.target.as_deref());
    eprintln!(
        "nros setup: {board}{} needs {} package(s):",
        args.target
            .as_deref()
            .map(|t| format!(" ({t})"))
            .unwrap_or_default(),
        packages.len()
    );

    let root = store_root();
    let lock_path = PathBuf::from("nros-sdk.lock");
    let mut lock = SdkLock::load(&lock_path)?;
    let mut installed = false;

    for name in &packages {
        // Only `[tool.*]` packages are installed into the store; `[source.*]`
        // build with the app, `[gated.*]` are user-installed.
        let Some(tool) = index.tool.get(*name) else {
            eprintln!("  {:<22} {}", name, disposition(&index, name, &host));
            continue;
        };
        let prefix = tool_prefix(&root, name, &tool.version);
        let action = plan_install(tool, &host, &prefix);
        eprintln!("  {:<22} {}", name, describe(&action, &tool.version, &host));

        if args.dry_run {
            continue;
        }
        match action {
            InstallAction::Unavailable => {
                bail!(
                    "nros setup: {name} {} has no prebuilt for {host} and no source recipe \
                     (add one to the index, or set up that host's toolchain manually)",
                    tool.version
                );
            }
            other => {
                let provenance = execute(&other, name, &tool.version, &prefix)
                    .wrap_err_with(|| format!("install {name} {}", tool.version))?;
                lock.record(name, &provenance);
                installed = true;
                eprintln!("    → {}", prefix.display());
            }
        }
    }

    if args.dry_run {
        eprintln!("(--dry-run: nothing installed)");
    } else if installed {
        lock.save(&lock_path)?;
        eprintln!(
            "nros setup: {board} ready; locked in {}",
            lock_path.display()
        );
    } else {
        eprintln!("nros setup: {board} — all packages already present");
    }
    Ok(())
}

/// Phase 187.6 — lazy install for `nros build` / `nros deploy`: resolve the
/// board's index tools and install any not already in the store, so a first
/// build/deploy needs no separate `nros setup` (the PlatformIO auto-install
/// ergonomic). Only `[tool.*]` packages are installed; `[source.*]` build with
/// the app and `[gated.*]` are user-provided. Opt out with `NROS_NO_AUTO_SETUP`.
/// No-op (empty) when no index is found; an unavailable tool warns rather than
/// fails so the downstream build surfaces the real miss (e.g. a system-installed
/// toolchain the index doesn't host).
///
/// Returns the `bin/` dirs of the resolved tools present in the store — Method A
/// callers ([`activate_store_path`]) prepend these to the env so every spawned
/// child finds the toolchain, without any non-`nros` script resolving paths.
pub fn ensure_tools(
    board: &str,
    target: Option<&str>,
    workspace: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    if std::env::var_os("NROS_NO_AUTO_SETUP").is_some() {
        return Ok(Vec::new());
    }
    let Some(index_path) = locate_index(workspace) else {
        return Ok(Vec::new());
    };
    let index = SdkIndex::load(&index_path)?;
    let host = host_key();
    let root = store_root();
    let lock_path = PathBuf::from("nros-sdk.lock");
    let mut lock = SdkLock::load(&lock_path)?;
    let mut installed = false;
    let mut bin_dirs = Vec::new();

    for name in resolve_packages(board, target) {
        let Some(tool) = index.tool.get(name) else {
            continue; // source / gated / not-in-index — not a store tool
        };
        let prefix = tool_prefix(&root, name, &tool.version);
        match plan_install(tool, &host, &prefix) {
            InstallAction::Present => {}
            InstallAction::Unavailable => {
                eprintln!(
                    "nros: {name} {} unavailable for {host} (no prebuilt, no source) — \
                     install it yourself if the build needs it",
                    tool.version
                );
                continue; // not in the store → nothing to add to PATH
            }
            action => {
                eprintln!(
                    "nros: auto-installing {name} {} (set NROS_NO_AUTO_SETUP to skip)",
                    tool.version
                );
                let prov = execute(&action, name, &tool.version, &prefix)
                    .wrap_err_with(|| format!("auto-setup {name} {}", tool.version))?;
                lock.record(name, &prov);
                installed = true;
                eprintln!("    → {}", prefix.display());
            }
        }
        let bin = prefix.join("bin");
        if bin.is_dir() {
            bin_dirs.push(bin);
        }
    }
    if installed {
        lock.save(&lock_path)?;
    }
    Ok(bin_dirs)
}

/// Method A — prepend the store `bin/` dirs (from [`ensure_tools`]) to this
/// process's `PATH` so every child `nros build`/`deploy` spawns (cargo, cmake,
/// west, the `build[]`/`package[]` steps) finds the toolchain on `PATH`. `nros`
/// is the single resolver; non-`nros` scripts/code never hunt for SDK paths.
/// A no-op when `dirs` is empty (no store tools / auto-setup skipped).
pub fn activate_store_path(dirs: &[PathBuf]) {
    if dirs.is_empty() {
        return;
    }
    let mut parts: Vec<PathBuf> = dirs.to_vec();
    if let Some(cur) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&cur));
    }
    if let Ok(joined) = std::env::join_paths(parts) {
        // SAFETY: a CLI invocation activating its own toolchain for the child
        // processes it is about to spawn; set before any thread reads the env.
        unsafe { std::env::set_var("PATH", joined) };
    }
}

/// Locate the SDK index for auto-setup: cwd, then the passed workspace, then
/// `$NROS_WORKSPACE`. `None` ⇒ auto-setup is a no-op (not every build runs near
/// a nano-ros workspace). Shared with `nros doctor`'s license-gate check (187.7).
pub(crate) fn locate_index(workspace: Option<&Path>) -> Option<PathBuf> {
    let cwd = PathBuf::from("nros-sdk-index.toml");
    if cwd.is_file() {
        return Some(cwd);
    }
    let ws = workspace
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("NROS_WORKSPACE").map(PathBuf::from));
    ws.map(|w| w.join("nros-sdk-index.toml"))
        .filter(|p| p.is_file())
}

/// One-line description of the planned action (mirrors `disposition`, but for an
/// already-resolved [`InstallAction`]).
fn describe(action: &InstallAction, version: &str, host: &str) -> String {
    match action {
        InstallAction::Present => format!("present {version} (skip)"),
        InstallAction::Prebuilt { .. } => format!("prebuilt {version} (dist {host})"),
        InstallAction::Source { .. } => format!("source build {version} (no prebuilt for {host})"),
        InstallAction::Unavailable => {
            format!("UNAVAILABLE {version} (no prebuilt for {host}, no source)")
        }
    }
}

/// Resolve a board (+ optional target triple) to the SDK package names it needs.
/// Heuristic on the target arch + board/platform keyword — the same knowledge
/// `profile()` encodes for the build; package *fetch* dispositions come from the
/// index (see [`disposition`]).
pub fn resolve_packages(board: &str, target: Option<&str>) -> Vec<&'static str> {
    let b = board.to_ascii_lowercase();
    let t = target.unwrap_or("").to_ascii_lowercase();
    let mut pkgs: Vec<&'static str> = Vec::new();

    // Cross-toolchain by target arch / board family.
    if t.contains("thumb")
        || (t.contains("arm") && !t.contains("linux"))
        || b.contains("cortex-m")
        || b.contains("cortex-r")
        || b.contains("stm32")
        || b.contains("mps2")
        || b.contains("orin")
    {
        pkgs.push("arm-none-eabi-gcc");
    } else if t.contains("riscv") || b.contains("riscv") {
        pkgs.push("riscv-none-elf-gcc");
    } else if t.contains("xtensa") || b.contains("esp32") {
        pkgs.push("esp-toolchain");
    }

    // QEMU for sim/test boards.
    if b.contains("qemu") || b.contains("mps2") || b.contains("native_sim") {
        pkgs.push("qemu");
    }

    // RTOS kernel / framework sources.
    if b.contains("freertos") {
        pkgs.push("freertos-kernel");
        pkgs.push("lwip");
    }
    if b.contains("threadx") {
        pkgs.push("threadx");
    }
    if b.contains("zephyr") || b.contains("native_sim") {
        pkgs.push("zephyr-sdk");
    }

    // Host router (run on the host for the Zenoh P2P path).
    if b == "native" || b == "posix" || t.contains("linux") {
        pkgs.push("zenohd");
    }

    // License-gated vendor SDKs.
    if b.contains("orin") {
        pkgs.push("nv-spe-fsp");
    }
    if b.contains("fvp") {
        pkgs.push("arm-fvp");
    }

    pkgs.sort_unstable();
    pkgs.dedup();
    pkgs
}

/// How `name` would be provisioned on `host`, per the index.
fn disposition(index: &SdkIndex, name: &str, host: &str) -> String {
    if let Some(tool) = index.tool.get(name) {
        if tool.dist_for(host).is_some() {
            format!("prebuilt {} (dist {host})", tool.version)
        } else if tool.source.is_some() {
            format!("source build {} (no prebuilt for {host})", tool.version)
        } else {
            format!(
                "UNAVAILABLE {} (no prebuilt for {host}, no source)",
                tool.version
            )
        }
    } else if let Some(src) = index.source.get(name) {
        format!("source {} (built with the app)", src.version)
    } else if let Some(g) = index.gated.get(name) {
        format!(
            "license-gated {} (set ${}{})",
            g.version,
            g.env,
            g.installer
                .as_deref()
                .map(|i| format!(", via {i}"))
                .unwrap_or_default()
        )
    } else {
        "NOT in index (add to nros-sdk-index.toml — 187.5)".to_string()
    }
}

fn print_list(index: &SdkIndex) {
    eprintln!("nros setup --list:");
    for (name, t) in &index.tool {
        eprintln!("  [tool]   {name:<22} {}", t.version);
    }
    for (name, s) in &index.source {
        eprintln!("  [source] {name:<22} {}", s.version);
    }
    for (name, g) in &index.gated {
        eprintln!("  [gated]  {name:<22} {} (${})", g.version, g.env);
    }
}

fn print_licenses(index: &SdkIndex) {
    if index.gated.is_empty() {
        eprintln!("nros setup --licenses: no license-gated packages");
        return;
    }
    eprintln!("nros setup --licenses (install these yourself; never fetched):");
    for (name, g) in &index.gated {
        eprintln!(
            "  {name:<16} {} — set ${}{}",
            g.version,
            g.env,
            g.installer
                .as_deref()
                .map(|i| format!(" (via {i})"))
                .unwrap_or_default()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_board_package_sets() {
        let arm_qemu = resolve_packages("mps2-an385-freertos", Some("thumbv7m-none-eabi"));
        assert!(arm_qemu.contains(&"arm-none-eabi-gcc"));
        assert!(arm_qemu.contains(&"qemu"));
        assert!(arm_qemu.contains(&"freertos-kernel") && arm_qemu.contains(&"lwip"));

        let riscv = resolve_packages("threadx-qemu-riscv64", Some("riscv64imac-unknown-none-elf"));
        assert!(riscv.contains(&"riscv-none-elf-gcc"));
        assert!(riscv.contains(&"qemu") && riscv.contains(&"threadx"));

        let esp = resolve_packages("esp32", None);
        assert_eq!(esp, vec!["esp-toolchain"]);

        let native = resolve_packages("native", Some("x86_64-unknown-linux-gnu"));
        assert_eq!(native, vec!["zenohd"]);

        let orin = resolve_packages("orin-spe", Some("armv7r-none-eabihf"));
        assert!(orin.contains(&"arm-none-eabi-gcc") && orin.contains(&"nv-spe-fsp"));
    }

    #[test]
    fn locate_index_falls_back_to_workspace() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = std::env::temp_dir().join(format!("nros_idx_{n}"));
        std::fs::create_dir_all(&ws).unwrap();
        // No index in the workspace yet → None (cwd has none under `cargo test`).
        assert_eq!(locate_index(Some(&ws)), None);
        // With one present → resolves to the workspace copy.
        let idx = ws.join("nros-sdk-index.toml");
        std::fs::write(&idx, "[tool.qemu]\nversion=\"1\"\n").unwrap();
        assert_eq!(locate_index(Some(&ws)), Some(idx));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn ensure_tools_noop_without_index() {
        // No index near a temp workspace ⇒ Ok no-op.
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = std::env::temp_dir().join(format!("nros_noidx_{n}"));
        std::fs::create_dir_all(&ws).unwrap();
        assert!(ensure_tools("native", None, Some(&ws)).is_ok());
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn disposition_reflects_index_state() {
        let idx = SdkIndex::parse(
            "[tool.qemu]\nversion=\"11.0\"\ndist.linux-x86_64={url=\"u\",sha256=\"h\"}\n\
             [tool.riscv-none-elf-gcc]\nversion=\"14\"\n[tool.riscv-none-elf-gcc.source]\ngit=\"g\"\nref=\"r\"\n\
             [source.freertos-kernel]\nversion=\"10.6.2\"\n\
             [gated.nv-spe-fsp]\nversion=\"36.3\"\nenv=\"NV_SPE_FSP_DIR\"\n",
        )
        .unwrap();
        assert!(disposition(&idx, "qemu", "linux-x86_64").starts_with("prebuilt"));
        assert!(disposition(&idx, "qemu", "macos-arm64").starts_with("UNAVAILABLE"));
        assert!(disposition(&idx, "riscv-none-elf-gcc", "macos-arm64").starts_with("source build"));
        assert!(disposition(&idx, "freertos-kernel", "linux-x86_64").starts_with("source "));
        assert!(disposition(&idx, "nv-spe-fsp", "linux-x86_64").starts_with("license-gated"));
        assert!(disposition(&idx, "openocd", "linux-x86_64").starts_with("NOT in index"));
    }
}
