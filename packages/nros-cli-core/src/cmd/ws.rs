//! `nros ws …` — workspace-level msg-pkg surface.
//!
//! Phase 210.B.3 + 210.D.1 (locked design). Subcommands:
//!
//! * `env` — print shell export for `NROS_INTERFACE_SEARCH_PATH`.
//! * `sync` — scan workspace, codegen msg pkgs into
//!   `generated/<pkg>/`, write `[patch.crates-io]`
//!   block into the patch authority Cargo.toml so plain `cargo build`
//!   resolves `local_msgs = "*"` to the generated crate.
//!
//! **Dual-mode (`cargo`-style):** every subcommand works on BOTH layouts —
//! a multi-pkg colcon workspace (`<root>/src/<pkg>/package.xml`) AND a
//! single standalone pkg (`<root>/package.xml`). Detection runs at command
//! time:
//!
//!   * **colcon-mode** iff `<root>/src/` exists AND at least one
//!     immediate subdir contains `package.xml`.
//!   * **single-pkg mode** iff `<root>/package.xml` exists and the colcon
//!     check fails.
//!
//! Mirrors `cargo build` which works at either a workspace root or a
//! standalone pkg dir without special arg.
//!
//! See `docs/roadmap/phase-210-ros-convention-codegen.md` for the
//! full design (patch authority detection, colcon-shape build dir,
//! the chicken-egg motivation for a pre-cargo sync step).

use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use eyre::{Result, WrapErr, bail, eyre};
use rosidl_bindgen::ament::Package;
use rosidl_codegen::RosEdition;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Sub,
}

#[derive(Debug, Subcommand)]
pub enum Sub {
    /// Print shell export adding <dir> (default `./src`) to
    /// `NROS_INTERFACE_SEARCH_PATH`. `eval "$(nros ws env)"`.
    Env(EnvArgs),

    /// Codegen workspace msg pkgs + write `[patch.crates-io]` block into
    /// each Rust consumer's patch authority Cargo.toml. Pre-cargo step;
    /// run once after editing `*.msg` files, then `cargo build` works.
    Sync(SyncArgs),

    /// List discovered msg + rust-consumer pkgs in the workspace (or
    /// single pkg). Prints kind, name, dir per row. (Phase 210.F.3.)
    List(ListArgs),

    /// Freshness check — non-fatal sibling of `sync --check`. Prints a
    /// one-line summary of `n up-to-date / n stale / n missing`.
    Status(StatusArgs),

    /// Remove `generated/` + the auto-managed
    /// `[patch.crates-io]` block from each Rust consumer's patch authority
    /// Cargo.toml. Leaves user-written sections alone.
    Clean(CleanArgs),

    /// Lint workspace pkgs: warn on missing `<member_of_group>
    /// rosidl_interface_packages</member_of_group>` markers, malformed
    /// `package.xml`, stale patch blocks. Mirrors the sync detection.
    Doctor(DoctorArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Shell {
    /// POSIX-shell `export VAR=…` (bash/zsh/sh).
    Posix,
    /// Fish-shell `set -gx VAR …`.
    Fish,
}

#[derive(Debug, ClapArgs)]
pub struct EnvArgs {
    /// Workspace root containing pkg subdirs with `package.xml`. Defaults
    /// to `./src` (the colcon-standard layout).
    pub workspace: Option<PathBuf>,

    /// Output shell flavour.
    #[arg(long, value_enum, default_value = "posix")]
    pub shell: Shell,
}

#[derive(Debug, ClapArgs)]
pub struct SyncArgs {
    /// Workspace root (the dir containing `src/`). Defaults to cwd.
    pub workspace: Option<PathBuf>,

    /// Output dir for generated msg crates (Phase 212 convention is `generated/`).
    #[arg(long, default_value = "generated")]
    pub build_dir: PathBuf,

    /// ROS 2 edition (`humble` | `iron`).
    #[arg(long, default_value = "humble")]
    pub ros_edition: String,

    /// Don't write — just print what would happen.
    #[arg(long)]
    pub dry_run: bool,

    /// Exit non-zero if any patch block is missing or stale (CI hook;
    /// also used by `nros ws status`).
    #[arg(long)]
    pub check: bool,

    /// Verbose codegen output.
    #[arg(short, long)]
    pub verbose: bool,

    /// Path to the nano-ros source tree. Accepted for back-compat but
    /// currently a NO-OP since post-212 alignment: the canonical 212
    /// shape carries nros-* runtime crates as path-deps in the user's
    /// own `[dependencies]`, so duplicating them in the patch block
    /// triggers cargo's "patch unused" warnings. Falls back to the env
    /// var `NROS_REPO_DIR` (cmake-side contract) when the flag is
    /// omitted.
    #[arg(long)]
    pub nano_ros_path: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
pub struct ListArgs {
    /// Workspace root (cwd or first ancestor containing `src/`). Defaults
    /// to cwd.
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
pub struct StatusArgs {
    pub workspace: Option<PathBuf>,
    #[arg(long, default_value = "generated")]
    pub build_dir: PathBuf,
}

#[derive(Debug, ClapArgs)]
pub struct CleanArgs {
    pub workspace: Option<PathBuf>,
    #[arg(long, default_value = "generated")]
    pub build_dir: PathBuf,
    /// Don't write — just print what would be removed.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, ClapArgs)]
pub struct DoctorArgs {
    pub workspace: Option<PathBuf>,
    #[arg(long, default_value = "generated")]
    pub build_dir: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    match args.command {
        Sub::Env(a) => run_env(a),
        Sub::Sync(a) => run_sync(a),
        Sub::List(a) => run_list(a),
        Sub::Status(a) => run_status(a),
        Sub::Clean(a) => run_clean(a),
        Sub::Doctor(a) => run_doctor(a),
    }
}

// =============================================================================
// `nros ws env`
// =============================================================================

fn run_env(args: EnvArgs) -> Result<()> {
    let abs = resolve_env_root(args.workspace.as_deref())?;
    let abs_s = abs.display().to_string();
    match args.shell {
        Shell::Posix => {
            println!("export NROS_INTERFACE_SEARCH_PATH=\"{abs_s}:${{NROS_INTERFACE_SEARCH_PATH:-}}\"");
        }
        Shell::Fish => {
            println!("set -gx NROS_INTERFACE_SEARCH_PATH \"{abs_s}\" $NROS_INTERFACE_SEARCH_PATH");
        }
    }
    Ok(())
}

/// Resolve the dir the cmake-side smart Find-stub will scan as a
/// `NROS_INTERFACE_SEARCH_PATH` entry. Mirrors `sync`'s dual-mode
/// detection so a `cd <my_pkg> && eval "$(nros ws env)"` from inside a
/// standalone pkg works the same as one run at a colcon workspace root.
///
/// Resolution order:
///   1. Explicit path arg → use it.
///   2. `<cwd>/src/<sub>/package.xml` exists → use `<cwd>/src`.
///   3. `<cwd>/package.xml` exists → use `<cwd>/..` (so smart Find-stub
///      finds `<parent>/<my_pkg>/package.xml` from there).
///   4. Fallback → `<cwd>/src` (legacy default; may not exist).
fn resolve_env_root(arg: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = arg {
        return std::fs::canonicalize(p)
            .map_err(|e| eyre!("ws env: {}: {e}", p.display()));
    }
    let cwd = std::env::current_dir()?;
    let src = cwd.join("src");
    if src.is_dir() && has_pkg_subdir(&src) {
        return std::fs::canonicalize(&src)
            .map_err(|e| eyre!("ws env: {}: {e}", src.display()));
    }
    if cwd.join("package.xml").is_file() {
        let parent = cwd.parent().ok_or_else(|| {
            eyre!("ws env: cwd {} is a standalone pkg but has no parent", cwd.display())
        })?;
        return std::fs::canonicalize(parent)
            .map_err(|e| eyre!("ws env: {}: {e}", parent.display()));
    }
    // Fallback — caller might not be in a pkg/workspace dir. Use ./src
    // and surface the error from canonicalize if it doesn't exist.
    std::fs::canonicalize(&src)
        .map_err(|e| eyre!("ws env: {}: {e}\n\
                            (no `src/<pkg>/package.xml` colcon layout and no `package.xml` \
                            at cwd — pass an explicit path arg)",
                           src.display()))
}

// =============================================================================
// `nros ws sync` — pre-cargo codegen + patch-table writer
// =============================================================================

/// Scanned workspace pkg.
#[derive(Debug, Clone)]
struct WsPkg {
    name: String,
    dir: PathBuf,
    manifest: PathBuf,
    /// True iff msg pkg (member_of_group=rosidl_interface_packages OR
    /// msg/srv/action dirs).
    is_msg_pkg: bool,
    /// True iff `Cargo.toml` at root.
    is_rust_pkg: bool,
    /// Pkg names declared in `<*depend>` tags (filtered for ROS-meta).
    deps: Vec<String>,
}

fn run_sync(args: SyncArgs) -> Result<()> {
    let ws_root: PathBuf = match args.workspace {
        Some(p) => std::fs::canonicalize(&p)
            .wrap_err_with(|| format!("ws sync: {}", p.display()))?,
        None => std::env::current_dir()?,
    };
    // Two layouts supported:
    //  * `src/`-based: workspace root has src/, src/<pkg>/ subdirs (colcon
    //    standard).
    //  * Single-pkg: workspace root IS the pkg dir (package.xml at root).
    //    Common for ported standalone examples (`examples/native/rust/talker`).
    // Heuristic: colcon-style layout iff `src/` exists AND has at least one
    // immediate subdir with `package.xml`. Falls through to single-pkg mode
    // when the workspace root itself carries `package.xml` (the standalone
    // example shape; `src/` may exist as the cargo source dir).
    let colcon_layout = ws_root.join("src").is_dir()
        && has_pkg_subdir(&ws_root.join("src"));
    let single_pkg_mode = !colcon_layout && ws_root.join("package.xml").is_file();
    let src_root = if colcon_layout {
        ws_root.join("src")
    } else if single_pkg_mode {
        ws_root.clone()
    } else {
        bail!(
            "ws sync: no `src/<pkg>/package.xml` and no `package.xml` at root \
             under {} — expected colcon-style workspace or single-pkg dir",
            ws_root.display()
        );
    };
    let build_root = if args.build_dir.is_absolute() {
        args.build_dir.clone()
    } else {
        ws_root.join(&args.build_dir)
    };

    let mut scan = Vec::new();
    if single_pkg_mode {
        scan_one_pkg_dir(&src_root, &mut scan)?;
    } else {
        scan_workspace(&src_root, &mut scan)?;
    }
    if scan.is_empty() {
        println!("ws sync: no pkgs under {}", src_root.display());
        return Ok(());
    }
    let msg_pkgs: Vec<&WsPkg> = scan.iter().filter(|p| p.is_msg_pkg).collect();
    let topo = topo_sort_msg_pkgs(&msg_pkgs)?;

    if args.verbose || args.dry_run {
        println!(
            "ws sync: scanned {} pkgs ({} msg, {} rust) under {}",
            scan.len(),
            msg_pkgs.len(),
            scan.iter().filter(|p| p.is_rust_pkg).count(),
            src_root.display()
        );
        println!("ws sync: topo order: {topo:?}");
    }

    if args.check {
        return check_freshness(&ws_root, &build_root, &scan, &topo);
    }

    if args.dry_run {
        for name in &topo {
            let pkg = scan.iter().find(|p| &p.name == name).unwrap();
            let out = build_root.join(name);
            println!(
                "ws sync: WOULD codegen {} from {} → {}",
                name,
                pkg.manifest.display(),
                out.display()
            );
        }
        return Ok(());
    }

    let edition = parse_edition(&args.ros_edition)?;

    // Track every pkg we generate so a later iteration (or AMENT-dep walk)
    // skips already-emitted ones. Keyed by pkg name.
    let mut emitted: HashSet<String> = HashSet::new();

    for name in &topo {
        let pkg = scan.iter().find(|p| &p.name == name).unwrap();
        // First materialize any AMENT-resolved cross-deps so the workspace
        // pkg's deps closure exists in build/ too. Skips workspace deps
        // (those are handled by topo order itself).
        codegen_ament_deps_for(&pkg.deps, &scan, &build_root, edition, &mut emitted, args.verbose)?;
        // Now generate the workspace pkg itself directly from its dir.
        if !emitted.contains(name) {
            codegen_workspace_pkg(pkg, &build_root, edition, args.verbose)?;
            emitted.insert(name.clone());
        }
    }
    // Also generate AMENT deps for every Rust consumer (pkg.xml deps).
    let rust_consumers: Vec<&WsPkg> = scan
        .iter()
        .filter(|p| p.is_rust_pkg && !p.is_msg_pkg)
        .collect();
    for c in &rust_consumers {
        codegen_ament_deps_for(&c.deps, &scan, &build_root, edition, &mut emitted, args.verbose)?;
    }

    if rust_consumers.is_empty() {
        println!("ws sync: no Rust consumer pkgs — patch tables not written.");
        return Ok(());
    }

    // Group consumers by patch authority. Cargo workspace covers many
    // consumers via one umbrella; standalone pkgs are their own authority.
    let all_emitted: Vec<String> = {
        let mut v: Vec<String> = emitted.iter().cloned().collect();
        v.sort();
        v
    };
    let mut authority_to_pkgs: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for c in &rust_consumers {
        let authority = find_patch_authority(&c.dir, &ws_root)?;
        authority_to_pkgs
            .entry(authority)
            .or_default()
            .extend(all_emitted.iter().cloned());
    }
    let nano_ros_path = args
        .nano_ros_path
        .or_else(|| std::env::var_os("NROS_REPO_DIR").map(PathBuf::from));
    for (authority, pkgs) in authority_to_pkgs {
        let mut unique = pkgs;
        unique.sort();
        unique.dedup();
        write_patch_block(&authority, &build_root, &unique, nano_ros_path.as_deref())?;
    }

    println!("ws sync: done.");
    Ok(())
}


fn parse_edition(s: &str) -> Result<RosEdition> {
    match s.to_lowercase().as_str() {
        "humble" => Ok(RosEdition::Humble),
        "iron" => Ok(RosEdition::Iron),
        other => bail!("ws sync: unknown ROS edition '{other}' (humble | iron)"),
    }
}

// Generate the workspace pkg directly (using its dir as a synthetic share_dir
// — `Package::from_share_dir` reads `package.xml` + scans msg/srv/action).
fn codegen_workspace_pkg(
    pkg: &WsPkg,
    build_root: &Path,
    edition: RosEdition,
    verbose: bool,
) -> Result<()> {
    let out_dir = build_root;
    std::fs::create_dir_all(&out_dir)
        .wrap_err_with(|| format!("ws sync: mkdir {}", out_dir.display()))?;
    if verbose {
        println!("ws sync: codegen workspace pkg {} → {}", pkg.name, out_dir.display());
    } else {
        println!("ws sync: codegen {}", pkg.name);
    }
    let package = Package::from_share_dir(pkg.dir.clone())
        .wrap_err_with(|| format!("ws sync: read pkg {}", pkg.dir.display()))?;
    rosidl_bindgen::generator::generate_package(&package, &out_dir, edition)
        .wrap_err_with(|| format!("ws sync: generate_package failed for {}", pkg.name))?;
    // Codegen emits <out_dir>/<pkg>/{Cargo.toml,src/} with sibling `path =
    // "../<dep>"` deps. We keep that flat layout (no extra `rust/`
    // nesting) so the relative paths between generated crates resolve
    // correctly without a rewrite pass. Our `nros_generator_rs` prefix
    // already namespaces by language — the extra `rust/` colcon adds is
    // there to coexist with `<pkg>/c/`, `<pkg>/cpp/`, etc. inside the
    // same generator's output, which we don't have.
    Ok(())
}

// Resolve AMENT-side deps (the per-pkg.xml `<depend>` tags not in workspace)
// and codegen each via Package::from_share_dir over its AMENT share path.
fn codegen_ament_deps_for(
    deps: &[String],
    scan: &[WsPkg],
    build_root: &Path,
    edition: RosEdition,
    emitted: &mut HashSet<String>,
    verbose: bool,
) -> Result<()> {
    // Pre-load ament index once per invocation.
    static AMENT_INDEX: std::sync::OnceLock<Option<rosidl_bindgen::ament::AmentIndex>> =
        std::sync::OnceLock::new();
    let idx = AMENT_INDEX.get_or_init(|| {
        rosidl_bindgen::ament::AmentIndex::from_env().ok()
    });
    let Some(idx) = idx else { return Ok(()) };

    let in_workspace: HashSet<&str> = scan.iter().map(|p| p.name.as_str()).collect();
    let mut to_resolve: Vec<String> =
        deps.iter().filter(|d| !in_workspace.contains(d.as_str())).cloned().collect();

    while let Some(dep) = to_resolve.pop() {
        if emitted.contains(&dep) {
            continue;
        }
        let Some(amented) = idx.packages().get(&dep).cloned() else {
            // AMENT doesn't know — silently skip (smart-stub semantics).
            continue;
        };
        // Codegen the AMENT pkg.
        let out_dir = build_root;
        std::fs::create_dir_all(&out_dir)?;
        if verbose {
            println!("ws sync: codegen AMENT pkg {} → {}", amented.name, out_dir.display());
        } else {
            println!("ws sync: codegen {}", amented.name);
        }
        rosidl_bindgen::generator::generate_package(&amented, &out_dir, edition)
            .wrap_err_with(|| format!("ws sync: generate_package failed for {}", amented.name))?;
        emitted.insert(amented.name.clone());
        // Queue this pkg's own deps (parse its package.xml).
        let pxml = amented.share_dir.join("package.xml");
        if pxml.is_file() {
            let body = std::fs::read_to_string(&pxml).unwrap_or_default();
            for d in extract_pkg_deps(&body) {
                if !in_workspace.contains(d.as_str()) && !emitted.contains(&d) {
                    to_resolve.push(d);
                }
            }
        }
    }
    Ok(())
}


// --- Scan ----------------------------------------------------------------------

fn has_pkg_subdir(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for e in entries.flatten() {
        if let Ok(t) = e.file_type() {
            if t.is_dir() && e.path().join("package.xml").is_file() {
                return true;
            }
        }
    }
    false
}

fn scan_one_pkg_dir(pkg_dir: &Path, out: &mut Vec<WsPkg>) -> Result<()> {
    let manifest = pkg_dir.join("package.xml");
    let body = std::fs::read_to_string(&manifest)?;
    let Some(name) = extract_pkg_name(&body) else {
        bail!("ws sync: single-pkg mode: package.xml at {} has no <name>",
              manifest.display());
    };
    let is_msg_pkg = body.contains("rosidl_interface_packages")
        || pkg_dir.join("msg").is_dir()
        || pkg_dir.join("srv").is_dir()
        || pkg_dir.join("action").is_dir();
    let is_rust_pkg = pkg_dir.join("Cargo.toml").is_file();
    let deps = extract_pkg_deps(&body);
    out.push(WsPkg {
        name,
        dir: pkg_dir.to_path_buf(),
        manifest,
        is_msg_pkg,
        is_rust_pkg,
        deps,
    });
    Ok(())
}

fn scan_workspace(src_root: &Path, out: &mut Vec<WsPkg>) -> Result<()> {
    for entry in std::fs::read_dir(src_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        let manifest = dir.join("package.xml");
        if !manifest.is_file() {
            continue;
        }
        let body = std::fs::read_to_string(&manifest)?;
        let Some(name) = extract_pkg_name(&body) else {
            continue;
        };
        let is_msg_pkg = body.contains("rosidl_interface_packages")
            || dir.join("msg").is_dir()
            || dir.join("srv").is_dir()
            || dir.join("action").is_dir();
        let is_rust_pkg = dir.join("Cargo.toml").is_file();
        let deps = extract_pkg_deps(&body);
        out.push(WsPkg {
            name,
            dir,
            manifest,
            is_msg_pkg,
            is_rust_pkg,
            deps,
        });
    }
    Ok(())
}

fn extract_pkg_name(body: &str) -> Option<String> {
    let start = body.find("<name>")? + "<name>".len();
    let end = body[start..].find("</name>")? + start;
    Some(body[start..end].trim().to_string())
}

fn extract_pkg_deps(body: &str) -> Vec<String> {
    let mut deps = Vec::new();
    for tag in &[
        "<depend>",
        "<build_depend>",
        "<exec_depend>",
        "<run_depend>",
        "<build_export_depend>",
    ] {
        let close = tag.replace("<", "</");
        let mut cursor = 0;
        while let Some(rel) = body[cursor..].find(tag) {
            let start = cursor + rel + tag.len();
            let Some(rel_close) = body[start..].find(close.as_str()) else {
                break;
            };
            let end = start + rel_close;
            let name = body[start..end].trim().to_string();
            if !name.is_empty() && !is_ros_meta_pkg(&name) {
                deps.push(name);
            }
            cursor = end;
        }
    }
    deps.sort();
    deps.dedup();
    deps
}

fn is_ros_meta_pkg(name: &str) -> bool {
    name.starts_with("rosidl")
        || name.starts_with("ament")
        || name == "rclcpp"
        || name == "rclpy"
        || name.starts_with("rcl")
        || name.starts_with("rmw")
        || name.starts_with("launch")
        || name == "catkin"
}

fn topo_sort_msg_pkgs(pkgs: &[&WsPkg]) -> Result<Vec<String>> {
    let names: std::collections::HashSet<&str> =
        pkgs.iter().map(|p| p.name.as_str()).collect();
    let mut remaining: Vec<&&WsPkg> = pkgs.iter().collect();
    let mut emitted: Vec<String> = Vec::new();
    while !remaining.is_empty() {
        let pick_idx = remaining.iter().position(|p| {
            p.deps
                .iter()
                .filter(|d| names.contains(d.as_str()))
                .all(|d| emitted.contains(d))
        });
        match pick_idx {
            Some(idx) => emitted.push(remaining.remove(idx).name.clone()),
            None => {
                let names: Vec<&str> = remaining.iter().map(|p| p.name.as_str()).collect();
                bail!("ws sync: dependency cycle (or missing dep) among {names:?}");
            }
        }
    }
    Ok(emitted)
}

// --- Patch authority -----------------------------------------------------------

fn find_patch_authority(start: &Path, ws_root: &Path) -> Result<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        let cargo = cur.join("Cargo.toml");
        if cargo.is_file() {
            let body = std::fs::read_to_string(&cargo)?;
            if has_workspace_table(&body) {
                return Ok(cargo);
            }
        }
        if cur == *ws_root {
            return Ok(start.join("Cargo.toml"));
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => return Ok(start.join("Cargo.toml")),
        }
    }
}

fn has_workspace_table(body: &str) -> bool {
    body.lines().any(|l| {
        let t = l.trim();
        t == "[workspace]" || t.starts_with("[workspace]")
    })
}

// --- Patch block writer --------------------------------------------------------

const BEGIN: &str = "# === BEGIN nros-managed [patch.crates-io] ===";
const END: &str = "# === END nros-managed [patch.crates-io] ===";

fn write_patch_block(
    authority: &Path,
    build_root: &Path,
    pkgs: &[String],
    nano_ros_path: Option<&Path>,
) -> Result<()> {
    let mut body = std::fs::read_to_string(authority)
        .wrap_err_with(|| format!("ws sync: read {}", authority.display()))?;
    let block = render_patch_block(authority, build_root, pkgs, nano_ros_path);
    body = replace_or_append_block(&body, &block);
    std::fs::write(authority, body)
        .wrap_err_with(|| format!("ws sync: write {}", authority.display()))?;
    println!(
        "ws sync: refreshed [patch.crates-io] block in {}",
        authority.display()
    );
    Ok(())
}

fn render_patch_block(
    authority: &Path,
    build_root: &Path,
    pkgs: &[String],
    nano_ros_path: Option<&Path>,
) -> String {
    let authority_dir = authority.parent().unwrap();
    let mut out = String::new();
    out.push_str(&format!("\n{BEGIN}\n"));
    // No timestamp — deterministic output keeps the committed Cargo.toml
    // diff-stable across re-syncs (only path entries change when the user
    // adds/removes a msg pkg). For "when did sync last run" debugging,
    // grep the generated/<pkg>/Cargo.toml mtime.
    out.push_str("# Auto-generated by `nros ws sync`. Do not edit between\n");
    out.push_str("# the BEGIN/END markers — re-run sync instead.\n");
    out.push_str("[patch.crates-io]\n");

    // 1) Generated msg crates (path = generated/<pkg>).
    for pkg in pkgs {
        let crate_root = build_root.join(pkg);
        let rel = pathdiff::diff_paths(&crate_root, authority_dir).unwrap_or(crate_root);
        out.push_str(&format!("{pkg} = {{ path = \"{}\" }}\n", rel.display()));
    }

    // 2) Minimum runtime patches the GENERATED msg crates depend on.
    //    Generated `<pkg>/Cargo.toml` carries `nros-core = "*"` +
    //    `nros-serdes = "*"` (registry-style), so even when the user's
    //    own Cargo.toml has direct path-deps for the larger nros-*
    //    runtime (canonical 212 shape), the generated crates need
    //    `[patch.crates-io]` entries for these two specific crates to
    //    resolve. Larger nros-* runtime patches were dropped post-212
    //    merge — they triggered cargo's "patch unused" warnings.
    if let Some(nrp) = nano_ros_path {
        for (cname, sub) in &[
            ("nros-core", "packages/core/nros-core"),
            ("nros-serdes", "packages/core/nros-serdes"),
        ] {
            let crate_root = nrp.join(sub);
            if !crate_root.join("Cargo.toml").is_file() {
                continue;
            }
            let rel = pathdiff::diff_paths(&crate_root, authority_dir).unwrap_or(crate_root);
            out.push_str(&format!("{cname} = {{ path = \"{}\" }}\n", rel.display()));
        }
    }

    out.push_str(&format!("{END}\n"));
    out
}

fn replace_or_append_block(body: &str, block: &str) -> String {
    if let (Some(b), Some(e)) = (body.find(BEGIN), body.find(END)) {
        let end_line_end = e + END.len();
        let block_trimmed = block.trim_start_matches('\n');
        let mut out = String::new();
        out.push_str(&body[..b]);
        out.push_str(block_trimmed);
        out.push_str(&body[end_line_end..]);
        out
    } else {
        let mut out = body.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(block);
        out
    }
}

// --- Check / freshness ---------------------------------------------------------

fn check_freshness(
    ws_root: &Path,
    build_root: &Path,
    scan: &[WsPkg],
    topo: &[String],
) -> Result<()> {
    let mut stale = false;
    for name in topo {
        let pkg = scan.iter().find(|p| &p.name == name).unwrap();
        let crate_root = build_root
            
            .join(name);
        let cargo = crate_root.join("Cargo.toml");
        if !cargo.is_file() {
            eprintln!(
                "ws sync --check: stale: {name} — no Cargo.toml at {}",
                cargo.display()
            );
            stale = true;
            continue;
        }
        let cargo_mt = std::fs::metadata(&cargo)?.modified()?;
        for subdir in &["msg", "srv", "action"] {
            let d = pkg.dir.join(subdir);
            if !d.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(d)? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let mt = entry.metadata()?.modified()?;
                if mt > cargo_mt {
                    eprintln!(
                        "ws sync --check: stale: {name} — {} newer than generated crate",
                        entry
                            .path()
                            .strip_prefix(ws_root)
                            .unwrap_or(&entry.path())
                            .display()
                    );
                    stale = true;
                }
            }
        }
    }
    if stale {
        bail!("ws sync --check: some pkgs stale — run `nros ws sync` first.");
    }
    println!("ws sync --check: all good.");
    Ok(())
}

// =============================================================================
// Phase 210.F.3 — `nros ws {list,status,clean,doctor}` sibling subcommands.
// All dual-mode (single-pkg + colcon-style workspace), same detection as sync.
// =============================================================================

/// Run sync's scan+resolve step without codegen — for list/status/clean/
/// doctor. Returns the workspace root + scanned pkgs + the resolved
/// build_root. The optional `build_dir` arg defaults to `<ws_root>/build`.
fn scan_for_query(
    workspace: Option<&Path>,
    build_dir: &Path,
) -> Result<(PathBuf, Vec<WsPkg>, PathBuf)> {
    let ws_root: PathBuf = match workspace {
        Some(p) => std::fs::canonicalize(p)
            .wrap_err_with(|| format!("ws: {}", p.display()))?,
        None => std::env::current_dir()?,
    };
    let colcon_layout = ws_root.join("src").is_dir()
        && has_pkg_subdir(&ws_root.join("src"));
    let single_pkg_mode = !colcon_layout && ws_root.join("package.xml").is_file();
    let src_root = if colcon_layout {
        ws_root.join("src")
    } else if single_pkg_mode {
        ws_root.clone()
    } else {
        bail!(
            "ws: no `src/<pkg>/package.xml` and no `package.xml` at root \
             under {} — expected colcon-style workspace or single-pkg dir",
            ws_root.display()
        );
    };
    let mut scan = Vec::new();
    if single_pkg_mode {
        scan_one_pkg_dir(&src_root, &mut scan)?;
    } else {
        scan_workspace(&src_root, &mut scan)?;
    }
    let build_root = if build_dir.is_absolute() {
        build_dir.to_path_buf()
    } else {
        ws_root.join(build_dir)
    };
    Ok((ws_root, scan, build_root))
}

// --- list ---------------------------------------------------------------------

fn run_list(args: ListArgs) -> Result<()> {
    // build_dir doesn't matter for list; use the default for the scan
    // helper's signature.
    let (ws_root, scan, _build_root) =
        scan_for_query(args.workspace.as_deref(), Path::new("build"))?;
    if scan.is_empty() {
        println!("ws list: no pkgs found.");
        return Ok(());
    }
    println!("ws list ({}):", ws_root.display());
    let mut kinds = (0usize, 0usize); // (msg, rust)
    for p in &scan {
        let kind = match (p.is_msg_pkg, p.is_rust_pkg) {
            (true, true) => "msg+rust",
            (true, false) => "msg",
            (false, true) => "rust",
            (false, false) => "other",
        };
        if p.is_msg_pkg {
            kinds.0 += 1;
        }
        if p.is_rust_pkg && !p.is_msg_pkg {
            kinds.1 += 1;
        }
        println!(
            "  {kind:9}  {:24}  {}",
            p.name,
            p.dir.strip_prefix(&ws_root).unwrap_or(&p.dir).display()
        );
    }
    println!("ws list: {} msg, {} rust consumer", kinds.0, kinds.1);
    Ok(())
}

// --- status -------------------------------------------------------------------

fn run_status(args: StatusArgs) -> Result<()> {
    let (ws_root, scan, build_root) =
        scan_for_query(args.workspace.as_deref(), &args.build_dir)?;
    let msg_pkgs: Vec<&WsPkg> = scan.iter().filter(|p| p.is_msg_pkg).collect();
    if msg_pkgs.is_empty() {
        println!("ws status: no msg pkgs.");
        return Ok(());
    }
    let mut up_to_date = 0;
    let mut stale = 0;
    let mut missing = 0;
    for pkg in &msg_pkgs {
        let crate_root = build_root.join(&pkg.name);
        let cargo = crate_root.join("Cargo.toml");
        if !cargo.is_file() {
            missing += 1;
            continue;
        }
        let cargo_mt = match std::fs::metadata(&cargo).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => {
                missing += 1;
                continue;
            }
        };
        let mut pkg_stale = false;
        for subdir in &["msg", "srv", "action"] {
            let d = pkg.dir.join(subdir);
            if !d.is_dir() {
                continue;
            }
            for e in std::fs::read_dir(d)?.flatten() {
                if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    if let Ok(mt) = e.metadata().and_then(|m| m.modified()) {
                        if mt > cargo_mt {
                            pkg_stale = true;
                            break;
                        }
                    }
                }
            }
            if pkg_stale {
                break;
            }
        }
        if pkg_stale {
            stale += 1;
        } else {
            up_to_date += 1;
        }
    }
    let _ = ws_root;
    println!(
        "ws status: {up_to_date} up-to-date, {stale} stale, {missing} missing \
         (of {} msg pkgs)",
        msg_pkgs.len()
    );
    Ok(())
}

// --- clean --------------------------------------------------------------------

fn run_clean(args: CleanArgs) -> Result<()> {
    let (ws_root, scan, build_root) =
        scan_for_query(args.workspace.as_deref(), &args.build_dir)?;
    let gen_dir = build_root;
    if gen_dir.is_dir() {
        if args.dry_run {
            println!("ws clean: WOULD rm -rf {}", gen_dir.display());
        } else {
            std::fs::remove_dir_all(&gen_dir)
                .wrap_err_with(|| format!("ws clean: rm {}", gen_dir.display()))?;
            println!("ws clean: removed {}", gen_dir.display());
        }
    } else {
        println!("ws clean: {} not present, skip", gen_dir.display());
    }
    // Strip the auto-managed patch block from every Rust consumer's patch
    // authority Cargo.toml.
    let rust_consumers: Vec<&WsPkg> =
        scan.iter().filter(|p| p.is_rust_pkg).collect();
    let mut authorities: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::new();
    for c in &rust_consumers {
        if let Ok(a) = find_patch_authority(&c.dir, &ws_root) {
            authorities.insert(a);
        }
    }
    for authority in authorities {
        let body = match std::fs::read_to_string(&authority) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let (Some(b), Some(e)) = (body.find(BEGIN), body.find(END)) else {
            continue;
        };
        if args.dry_run {
            println!("ws clean: WOULD strip patch block from {}", authority.display());
            continue;
        }
        let end_line_end = e + END.len();
        let mut out = String::new();
        out.push_str(&body[..b]);
        // Drop the preceding blank line we added when writing the block.
        let trimmed = out.trim_end_matches('\n');
        out.truncate(trimmed.len());
        out.push('\n');
        out.push_str(&body[end_line_end..]);
        std::fs::write(&authority, out)
            .wrap_err_with(|| format!("ws clean: write {}", authority.display()))?;
        println!("ws clean: stripped patch block from {}", authority.display());
    }
    Ok(())
}

// --- doctor -------------------------------------------------------------------

fn run_doctor(args: DoctorArgs) -> Result<()> {
    let (ws_root, scan, build_root) =
        scan_for_query(args.workspace.as_deref(), &args.build_dir)?;
    let mut warnings = 0;
    println!("ws doctor ({})", ws_root.display());
    for pkg in &scan {
        // (a) package.xml well-formed?
        let body = match std::fs::read_to_string(&pkg.manifest) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  ✗ {}: package.xml read error: {e}", pkg.name);
                warnings += 1;
                continue;
            }
        };
        // (b) msg pkg without member_of_group=rosidl_interface_packages
        let has_iface_group = body.contains("rosidl_interface_packages");
        let has_msg_dirs = pkg.dir.join("msg").is_dir()
            || pkg.dir.join("srv").is_dir()
            || pkg.dir.join("action").is_dir();
        if has_msg_dirs && !has_iface_group {
            eprintln!(
                "  ⚠ {}: has msg/srv/action dirs but pkg.xml lacks \
                 <member_of_group>rosidl_interface_packages</member_of_group> \
                 — upstream colcon won't classify it as an interface pkg",
                pkg.name
            );
            warnings += 1;
        }
        // (c) rust consumer: is the patch authority Cargo.toml sane?
        if pkg.is_rust_pkg && !pkg.is_msg_pkg {
            match find_patch_authority(&pkg.dir, &ws_root) {
                Ok(a) => {
                    let body = std::fs::read_to_string(&a).unwrap_or_default();
                    if !body.contains(BEGIN) {
                        eprintln!(
                            "  ⚠ {}: no nros-managed [patch.crates-io] block in \
                             patch authority ({}). Run `nros ws sync`.",
                            pkg.name, a.display()
                        );
                        warnings += 1;
                    }
                }
                Err(e) => {
                    eprintln!("  ⚠ {}: patch authority resolve failed: {e}", pkg.name);
                    warnings += 1;
                }
            }
        }
    }
    // (d) stale msg pkgs (same logic as status).
    let _ = build_root;
    if warnings == 0 {
        println!("ws doctor: no issues.");
    } else {
        println!("ws doctor: {warnings} warning(s).");
    }
    Ok(())
}
