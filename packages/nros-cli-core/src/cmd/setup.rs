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
    /// Board to set up (resolves its toolchain/SDK package set from the index
    /// `[board.*]` table).
    pub board: Option<String>,

    /// List every package in the index + its version.
    #[arg(long)]
    pub list: bool,

    /// Show the license-gated packages + how to install them.
    #[arg(long)]
    pub licenses: bool,

    /// Install a single tool by name (instead of a board's whole set), e.g.
    /// `--tool qemu`. The `just <module> setup` recipes call this.
    #[arg(long)]
    pub tool: Option<String>,

    /// Install prefix override (only with `--tool`): place the tool here instead
    /// of the shared store, e.g. `--prefix build/qemu` so the test harness finds
    /// it where it already looks. Layout is identical (`<prefix>/bin/…`).
    #[arg(long)]
    pub prefix: Option<PathBuf>,

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

    if let Some(tool) = args.tool.as_deref() {
        return install_single_tool(&index, tool, args.prefix.as_deref(), args.dry_run);
    }

    let board = match args.board.as_deref() {
        Some(b) => b,
        None => {
            bail!("nros setup: give a <board>, `--tool <name>`, `--list`, or `--licenses`")
        }
    };

    let packages = resolve_packages(&index, board)?;
    eprintln!("nros setup: {board} needs {} package(s):", packages.len());

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

/// Install one tool by name (`nros setup --tool <name>`). `prefix_override`
/// (from `--prefix`) places it outside the shared store — e.g. `build/qemu`, the
/// location the test harness already reads, so `just <module> setup` can delegate
/// here with no harness change and no script-side path resolution. Prebuilt-or-
/// source per the index (187.3); the lockfile is only updated for shared-store
/// installs (a `--prefix` placement is workspace-local).
fn install_single_tool(
    index: &SdkIndex,
    name: &str,
    prefix_override: Option<&Path>,
    dry_run: bool,
) -> Result<()> {
    let host = host_key();
    let tool = index
        .tool
        .get(name)
        .ok_or_else(|| eyre::eyre!("nros setup --tool: no [tool.{name}] in the index"))?;
    let root = store_root();
    let prefix = prefix_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| tool_prefix(&root, name, &tool.version));

    let action = plan_install(tool, &host, &prefix);
    eprintln!(
        "nros setup --tool {name}: {} → {}",
        describe(&action, &tool.version, &host),
        prefix.display()
    );
    if dry_run {
        eprintln!("(--dry-run: nothing installed)");
        return Ok(());
    }
    match action {
        InstallAction::Present => {}
        InstallAction::Unavailable => bail!(
            "nros setup --tool {name} {}: no prebuilt for {host} and no source recipe",
            tool.version
        ),
        other => {
            let prov = execute(&other, name, &tool.version, &prefix)
                .wrap_err_with(|| format!("install {name} {}", tool.version))?;
            // Only the shared store is tracked by the lock; --prefix is local.
            if prefix_override.is_none() {
                let lock_path = PathBuf::from("nros-sdk.lock");
                let mut lock = SdkLock::load(&lock_path)?;
                lock.record(name, &prov);
                lock.save(&lock_path)?;
            }
        }
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
pub fn ensure_tools(board: &str, workspace: Option<&Path>) -> Result<Vec<PathBuf>> {
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

    // Unknown board ⇒ no known package set — warn + skip (lazy auto-setup is
    // best-effort; the user provides tools). `nros setup` errors instead.
    let packages = match resolve_packages(&index, board) {
        Ok(p) => p,
        Err(_) => {
            eprintln!(
                "nros: board '{board}' not in the SDK index — skipping auto-setup \
                 (provide its tools yourself, or add a [board.{board}] entry)"
            );
            return Ok(Vec::new());
        }
    };
    for name in packages {
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

/// Resolve a board to its SDK package set from the index `[board.*]` table — the
/// board→toolchain SSOT (Phase 191.1). No board-name guessing: an unknown board
/// is a clear error listing the known boards, not a silent wrong package set
/// (the failure mode the old keyword heuristic had — it mis-resolved ESP32-C3 as
/// Xtensa). Adding a board is a `[board.<name>]` entry, no code change.
pub fn resolve_packages<'i>(index: &'i SdkIndex, board: &str) -> Result<Vec<&'i str>> {
    match index.board.get(board) {
        Some(entry) => Ok(entry.packages.iter().map(String::as_str).collect()),
        None => {
            let mut known: Vec<&str> = index.board.keys().map(String::as_str).collect();
            known.sort_unstable();
            bail!(
                "nros setup: unknown board '{board}'. Known boards: {}. \
                 Add a [board.{board}] entry to nros-sdk-index.toml.",
                if known.is_empty() {
                    "(none in index)".to_string()
                } else {
                    known.join(", ")
                }
            )
        }
    }
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

    fn board_index() -> SdkIndex {
        SdkIndex::parse(
            "[board.qemu-arm-freertos]\npackages=[\"arm-none-eabi-gcc\",\"qemu\",\"freertos-kernel\",\"lwip\"]\n\
             [board.qemu-riscv64-threadx]\npackages=[\"riscv-none-elf-gcc\",\"qemu\",\"threadx\"]\n\
             [board.esp32]\narch=\"riscv32\"\npackages=[]\n\
             [board.native]\npackages=[\"zenohd\"]\n\
             [board.orin-spe]\npackages=[\"arm-none-eabi-gcc\",\"nv-spe-fsp\"]\n",
        )
        .unwrap()
    }

    #[test]
    fn resolves_board_package_sets_from_index() {
        let idx = board_index();
        let fr = resolve_packages(&idx, "qemu-arm-freertos").unwrap();
        assert!(fr.contains(&"arm-none-eabi-gcc") && fr.contains(&"qemu"));
        assert!(fr.contains(&"freertos-kernel") && fr.contains(&"lwip"));

        let tx = resolve_packages(&idx, "qemu-riscv64-threadx").unwrap();
        assert!(tx.contains(&"riscv-none-elf-gcc") && tx.contains(&"qemu") && tx.contains(&"threadx"));

        // ESP32-C3: declared arch riscv32, no index host-tool (rustup target).
        assert!(resolve_packages(&idx, "esp32").unwrap().is_empty());
        assert_eq!(resolve_packages(&idx, "native").unwrap(), vec!["zenohd"]);
        let orin = resolve_packages(&idx, "orin-spe").unwrap();
        assert!(orin.contains(&"arm-none-eabi-gcc") && orin.contains(&"nv-spe-fsp"));

        // Unknown board → error (no silent wrong guess), lists known boards.
        let err = resolve_packages(&idx, "totally-unknown").unwrap_err().to_string();
        assert!(err.contains("unknown board") && err.contains("native"));
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
        assert!(ensure_tools("native", Some(&ws)).is_ok());
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
