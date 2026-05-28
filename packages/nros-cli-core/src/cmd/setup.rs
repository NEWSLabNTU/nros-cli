//! `nros setup` — Phase 187.2: resolve a board's toolchain/SDK package set from
//! the index and report the install plan. The actual fetch / source-build /
//! cache is Phase 187.3; this verb does the CLI + board→package resolution +
//! `--list` / `--licenses` / the per-host disposition plan.
//!
//! See `docs/design/nros-setup-toolchain-management.md`.

use std::path::PathBuf;

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
