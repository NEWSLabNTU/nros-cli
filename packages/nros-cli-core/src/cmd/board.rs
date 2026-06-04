//! `nros board` — board crate introspection.
//!
//! * `list` — Phase 111.A.8: enumerate every `nros-board-*` crate under
//!   `<workspace>/packages/boards/`.
//! * `info <name>` — Phase 215.C.3: print the side-by-side `Cargo.toml` +
//!   `board.cmake` views of a board crate's manifest, optionally erroring
//!   when the two faces drift (the Phase 215.F audit hook).

use clap::{Args as ClapArgs, Subcommand};
use eyre::{Result, WrapErr, eyre};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::orchestration::board_metadata::{BoardMetadata, parse_board_metadata};

#[derive(Debug, Subcommand)]
pub enum Args {
    /// List every supported board crate
    List(ListArgs),
    /// Print a board crate's Cargo.toml + board.cmake views side-by-side
    /// (Phase 215.C.3).
    Info(InfoArgs),
}

#[derive(Debug, ClapArgs)]
pub struct ListArgs {
    /// Path to the nano-ros workspace root (auto-detected by walking
    /// upward from cwd if omitted)
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
pub struct InfoArgs {
    /// Board crate suffix (after the `nros-board-` prefix). E.g.
    /// `fvp-aemv8r-smp` resolves to `packages/boards/nros-board-fvp-aemv8r-smp/`.
    pub name: String,
    /// Path to the nano-ros workspace root (auto-detected by walking
    /// upward from cwd if omitted). May also be set via
    /// `NROS_WORKSPACE_ROOT`.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// Exit with status 1 if drift is detected between the Cargo.toml
    /// view and the board.cmake view. When only one source is present
    /// (e.g. a bare board with no board.cmake), exits 0 — there is
    /// nothing to drift against.
    #[arg(long)]
    pub check_drift: bool,
}

pub fn run(args: Args) -> Result<()> {
    match args {
        Args::List(args) => list(args),
        Args::Info(args) => info(args),
    }
}

fn list(args: ListArgs) -> Result<()> {
    let root = match args.workspace {
        Some(p) => p,
        None => find_workspace_root()?,
    };
    let boards_dir = root.join("packages").join("boards");
    if !boards_dir.is_dir() {
        return Err(eyre!(
            "no `packages/boards/` directory under {}",
            root.display()
        ));
    }

    let mut entries: Vec<BoardEntry> = Vec::new();
    for entry in fs::read_dir(&boards_dir)
        .wrap_err_with(|| format!("failed to read {}", boards_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let cargo_toml = path.join("Cargo.toml");
        if !cargo_toml.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("nros-board-") {
            continue;
        }
        match read_board(&cargo_toml) {
            Ok(b) => entries.push(b),
            Err(e) => eprintln!("warning: skipping {}: {e}", name),
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    if entries.is_empty() {
        println!("No board crates found under {}", boards_dir.display());
        return Ok(());
    }

    let name_w = entries
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!("{:<name_w$}  description", "name", name_w = name_w);
    println!(
        "{:<name_w$}  {}",
        "-".repeat(name_w),
        "-".repeat(60),
        name_w = name_w
    );
    for b in entries {
        println!("{:<name_w$}  {}", b.name, b.description, name_w = name_w);
    }
    Ok(())
}

struct BoardEntry {
    name: String,
    description: String,
}

fn read_board(cargo_toml: &Path) -> Result<BoardEntry> {
    let raw = fs::read_to_string(cargo_toml)?;
    let doc: toml_edit::DocumentMut = raw.parse()?;
    let pkg = doc
        .get("package")
        .and_then(|p| p.as_table())
        .ok_or_else(|| eyre!("no [package] table in {}", cargo_toml.display()))?;
    let name = pkg
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| eyre!("no [package].name in {}", cargo_toml.display()))?
        .to_string();
    let description = pkg
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    Ok(BoardEntry { name, description })
}

// -----------------------------------------------------------------------
// Phase 215.C.3 — `nros board info <name>`
// -----------------------------------------------------------------------

/// JSON envelope produced by `nros board info`.
#[derive(Debug, Serialize)]
struct BoardInfo {
    name: String,
    crate_dir: PathBuf,
    cargo_metadata: Option<BoardMetadata>,
    board_cmake: Option<BTreeMap<String, String>>,
    drift: Vec<DriftEntry>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DriftEntry {
    pub(crate) field: &'static str,
    pub(crate) cargo_metadata: String,
    pub(crate) board_cmake: String,
}

fn info(args: InfoArgs) -> Result<()> {
    let root = match args.workspace {
        Some(p) => p,
        None => find_workspace_root()?,
    };
    let crate_dir = locate_board_crate(&root, &args.name)?;
    let cargo_toml = crate_dir.join("Cargo.toml");
    let board_cmake_path = crate_dir.join("board.cmake");

    let cargo_metadata = if cargo_toml.is_file() {
        match parse_board_metadata(&cargo_toml) {
            Ok(m) => Some(m),
            Err(e) => {
                // Surface the diagnostic on stderr so users see WHY the
                // Cargo.toml face was skipped, but keep the info dump
                // useful when only board.cmake is authored yet.
                eprintln!("warning: {e}");
                None
            }
        }
    } else {
        None
    };

    let board_cmake = if board_cmake_path.is_file() {
        let raw = fs::read_to_string(&board_cmake_path)
            .wrap_err_with(|| format!("read {}", board_cmake_path.display()))?;
        Some(parse_board_cmake(&raw))
    } else {
        None
    };

    let drift = match (&cargo_metadata, &board_cmake) {
        (Some(c), Some(k)) => compute_drift(c, k),
        _ => Vec::new(),
    };

    let info = BoardInfo {
        name: args.name.clone(),
        crate_dir: crate_dir.clone(),
        cargo_metadata,
        board_cmake,
        drift,
    };
    let json =
        serde_json::to_string_pretty(&info).wrap_err("serialise BoardInfo as JSON")?;
    println!("{json}");

    if args.check_drift && !info.drift.is_empty() {
        return Err(eyre!(
            "drift detected between Cargo.toml and board.cmake for `{}` \
             ({} field(s))",
            args.name,
            info.drift.len()
        ));
    }
    Ok(())
}

/// Resolve `packages/boards/nros-board-<name>/` under the workspace
/// root. The board crate dir name is `nros-board-<name>` verbatim;
/// `name = "fvp-aemv8r-smp"` ⇒ `packages/boards/nros-board-fvp-aemv8r-smp/`.
fn locate_board_crate(workspace_root: &Path, name: &str) -> Result<PathBuf> {
    let dir_name = format!("nros-board-{name}");
    let dir = workspace_root.join("packages").join("boards").join(&dir_name);
    if !dir.is_dir() {
        return Err(eyre!(
            "no board crate dir `{}` under `{}/packages/boards/`",
            dir_name,
            workspace_root.display()
        ));
    }
    Ok(dir)
}

/// Tokenise `board.cmake` and return a `name → value` map for every
/// `set(NROS_BOARD_<KEY> <value> …)` call. Values are returned with
/// surrounding quotes stripped. Cache-suffix annotations like
/// `CACHE STRING "..."` are tolerated — only the first whitespace-
/// separated value after the variable name is captured (the variable's
/// value, not the cache descriptor). For semicolon-delimited lists the
/// raw form is preserved; downstream consumers can `.split(';')` as
/// needed.
pub(crate) fn parse_board_cmake(source: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for raw_line in source.lines() {
        // Strip `#` comments. `set()` calls don't legitimately contain `#`
        // outside of a quoted value; the board.cmake schema disallows it.
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        // Match `set(NROS_BOARD_<KEY> …)` — case-insensitive cmake `set`.
        let lower = line.to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix("set(") else {
            continue;
        };
        // Re-slice the original line at the same offset so case + value
        // are preserved.
        let after_set = &line["set(".len()..];
        // Strip trailing `)` if present on the same line.
        let body = after_set.trim_end_matches(')').trim();
        let _ = rest; // marker used only to confirm prefix

        let mut tokens = tokenise_cmake_args(body);
        let var = tokens.next().unwrap_or_default();
        if !var.starts_with("NROS_BOARD_") {
            continue;
        }
        // Take the first value token (skipping cmake metadata keywords
        // like CACHE / FORCE / PARENT_SCOPE). The board.cmake schema
        // (Phase 215.A.1) is plain `set(NROS_BOARD_X "value")` — but
        // tolerate the cmake `CACHE STRING "doc"` shape too.
        let mut value: Option<String> = None;
        for tok in tokens.by_ref() {
            let upper = tok.to_ascii_uppercase();
            if matches!(upper.as_str(), "CACHE" | "FORCE" | "PARENT_SCOPE") {
                continue;
            }
            // Treat `STRING` / `BOOL` / `PATH` / `FILEPATH` / `INTERNAL` as
            // cache-type markers when they appear AFTER CACHE; if value is
            // already set, stop.
            if value.is_some() {
                break;
            }
            value = Some(tok);
        }
        if let Some(v) = value {
            out.insert(var, v);
        }
    }
    out
}

/// Minimal CMake `set()` argument tokeniser. Handles bare words and
/// double-quoted strings; CMake's bracket syntax `[[…]]` is NOT
/// supported (the board.cmake schema doesn't use it).
fn tokenise_cmake_args(s: &str) -> impl Iterator<Item = String> + '_ {
    let mut iter = s.chars().peekable();
    std::iter::from_fn(move || {
        // Skip whitespace.
        while let Some(&c) = iter.peek() {
            if c.is_whitespace() {
                iter.next();
            } else {
                break;
            }
        }
        let first = iter.next()?;
        let mut tok = String::new();
        if first == '"' {
            // Quoted string — capture until unescaped closing quote.
            while let Some(c) = iter.next() {
                if c == '\\' {
                    if let Some(esc) = iter.next() {
                        tok.push(esc);
                    }
                } else if c == '"' {
                    return Some(tok);
                } else {
                    tok.push(c);
                }
            }
            // Unterminated quoted string — return what we have.
            Some(tok)
        } else {
            tok.push(first);
            while let Some(&c) = iter.peek() {
                if c.is_whitespace() {
                    break;
                }
                tok.push(c);
                iter.next();
            }
            Some(tok)
        }
    })
}

/// Map a `BoardMetadata` field → the `board.cmake` variable name it
/// mirrors (Phase 215.A.1).
const FIELD_MAP: &[(&str, &str)] = &[
    ("zephyr_board", "NROS_BOARD_ZEPHYR_ID"),
    ("toolchain", "NROS_BOARD_TOOLCHAIN"),
    ("default_rmw", "NROS_BOARD_DEFAULT_RMW"),
    ("default_transport", "NROS_BOARD_DEFAULT_TRANSPORT"),
    ("runner", "NROS_BOARD_RUNNER"),
    ("prj_conf", "NROS_BOARD_PRJ_CONF"),
    ("board_conf", "NROS_BOARD_BOARD_CONF"),
    ("board_overlay", "NROS_BOARD_BOARD_OVERLAY"),
];

/// Compare the typed Cargo.toml view to the parsed board.cmake map.
///
/// Path-shaped fields (`prj_conf`, `board_conf`, `board_overlay`) are
/// compared by basename — the Cargo.toml face stores them relative to
/// `Cargo.toml`, while the cmake face stores absolute paths post-
/// resolution. Comparing the trailing path segment keeps the audit
/// meaningful without requiring the runner to canonicalise both
/// surfaces.
pub(crate) fn compute_drift(
    cargo: &BoardMetadata,
    cmake: &BTreeMap<String, String>,
) -> Vec<DriftEntry> {
    let mut out = Vec::new();
    for &(field, cmake_var) in FIELD_MAP {
        let cargo_val = cargo_field(cargo, field);
        let Some(cmake_val) = cmake.get(cmake_var) else {
            continue; // board.cmake doesn't author this field — not drift.
        };
        let (lhs, rhs) = match field {
            "prj_conf" | "board_conf" | "board_overlay" => (
                basename(&cargo_val).to_string(),
                basename(cmake_val).to_string(),
            ),
            _ => (cargo_val.clone(), cmake_val.clone()),
        };
        if lhs != rhs {
            out.push(DriftEntry {
                field,
                cargo_metadata: cargo_val,
                board_cmake: cmake_val.clone(),
            });
        }
    }
    // `gated` is a semicolon-list in cmake; compare as sorted set.
    if let Some(cmake_gated) = cmake.get("NROS_BOARD_GATED_PKGS") {
        let mut cargo_gated = cargo.gated.clone();
        cargo_gated.sort();
        let mut cmake_gated_v: Vec<String> = cmake_gated
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        cmake_gated_v.sort();
        if cargo_gated != cmake_gated_v {
            out.push(DriftEntry {
                field: "gated",
                cargo_metadata: cargo.gated.join(";"),
                board_cmake: cmake_gated.clone(),
            });
        }
    }
    out
}

fn cargo_field(m: &BoardMetadata, field: &str) -> String {
    match field {
        "zephyr_board" => m.zephyr_board.clone(),
        "toolchain" => m.toolchain.clone(),
        "default_rmw" => m.default_rmw.clone(),
        "default_transport" => m.default_transport.clone(),
        "runner" => m.runner.clone(),
        "prj_conf" => m.prj_conf.clone(),
        "board_conf" => m.board_conf.clone(),
        "board_overlay" => m.board_overlay.clone(),
        other => panic!("cargo_field: unknown field {other}"),
    }
}

fn basename(p: &str) -> &str {
    p.rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(p)
}

/// Walk upward from cwd until a directory containing `packages/boards/`
/// is found. The `NROS_WORKSPACE_ROOT` env var, when set, short-
/// circuits the walk (matches `nros_build::pkg_index::detect_workspace_root`).
pub(crate) fn find_workspace_root() -> Result<PathBuf> {
    if let Some(override_) = std::env::var_os("NROS_WORKSPACE_ROOT") {
        let p = PathBuf::from(override_);
        if !p.exists() {
            return Err(eyre!(
                "NROS_WORKSPACE_ROOT=`{}` does not exist on disk",
                p.display()
            ));
        }
        return Ok(p);
    }
    let cwd = std::env::current_dir()?;
    let mut cur: &Path = &cwd;
    loop {
        if cur.join("packages").join("boards").is_dir() {
            return Ok(cur.to_path_buf());
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => {
                return Err(eyre!(
                    "could not auto-detect nano-ros workspace root from {}; \
                     pass --workspace <path> explicitly",
                    cwd.display()
                ));
            }
        }
    }
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_board_cmake_basic() {
        let src = r#"
# Phase 215.A.2 — FVP board manifest
set(NROS_BOARD_ZEPHYR_ID "fvp_baser_aemv8r/fvp_aemv8r_aarch64/smp")
set(NROS_BOARD_TOOLCHAIN "aarch64-zephyr-elf")
set(NROS_BOARD_GATED_PKGS "arm-fvp")
set(NROS_BOARD_DEFAULT_RMW "cyclonedds")
set(NROS_BOARD_DEFAULT_TRANSPORT "ethernet")
set(NROS_BOARD_RUNNER "armfvp")
set(NROS_BOARD_PRJ_CONF "${CMAKE_CURRENT_LIST_DIR}/prj.conf")
set(NROS_BOARD_BOARD_CONF "${CMAKE_CURRENT_LIST_DIR}/boards/x.conf")
set(NROS_BOARD_BOARD_OVERLAY "${CMAKE_CURRENT_LIST_DIR}/boards/x.overlay")
"#;
        let m = parse_board_cmake(src);
        assert_eq!(
            m.get("NROS_BOARD_ZEPHYR_ID").map(String::as_str),
            Some("fvp_baser_aemv8r/fvp_aemv8r_aarch64/smp")
        );
        assert_eq!(m.get("NROS_BOARD_RUNNER").map(String::as_str), Some("armfvp"));
        assert!(m.contains_key("NROS_BOARD_GATED_PKGS"));
    }

    #[test]
    fn parses_board_cmake_cache_variant() {
        // `CACHE STRING "doc"` tail must not confuse the tokeniser.
        let src = r#"
set(NROS_BOARD_RUNNER "armfvp" CACHE STRING "runner")
"#;
        let m = parse_board_cmake(src);
        assert_eq!(m.get("NROS_BOARD_RUNNER").map(String::as_str), Some("armfvp"));
    }

    #[test]
    fn drift_compute_agreement() {
        let cargo = BoardMetadata {
            zephyr_board: "fvp_baser_aemv8r/fvp_aemv8r_aarch64/smp".into(),
            toolchain: "aarch64-zephyr-elf".into(),
            gated: vec!["arm-fvp".into()],
            default_rmw: "cyclonedds".into(),
            default_transport: "ethernet".into(),
            runner: "armfvp".into(),
            prj_conf: "prj.conf".into(),
            board_conf: "boards/x.conf".into(),
            board_overlay: "boards/x.overlay".into(),
        };
        let mut cmake: BTreeMap<String, String> = BTreeMap::new();
        cmake.insert(
            "NROS_BOARD_ZEPHYR_ID".into(),
            "fvp_baser_aemv8r/fvp_aemv8r_aarch64/smp".into(),
        );
        cmake.insert("NROS_BOARD_TOOLCHAIN".into(), "aarch64-zephyr-elf".into());
        cmake.insert("NROS_BOARD_GATED_PKGS".into(), "arm-fvp".into());
        cmake.insert("NROS_BOARD_DEFAULT_RMW".into(), "cyclonedds".into());
        cmake.insert("NROS_BOARD_DEFAULT_TRANSPORT".into(), "ethernet".into());
        cmake.insert("NROS_BOARD_RUNNER".into(), "armfvp".into());
        cmake.insert(
            "NROS_BOARD_PRJ_CONF".into(),
            "/abs/path/to/prj.conf".into(),
        );
        cmake.insert("NROS_BOARD_BOARD_CONF".into(), "/abs/path/to/x.conf".into());
        cmake.insert(
            "NROS_BOARD_BOARD_OVERLAY".into(),
            "/abs/path/to/x.overlay".into(),
        );
        let drift = compute_drift(&cargo, &cmake);
        assert!(drift.is_empty(), "no drift expected: {drift:?}");
    }

    #[test]
    fn drift_compute_runner_mismatch() {
        let cargo = BoardMetadata {
            zephyr_board: "x".into(),
            toolchain: "y".into(),
            gated: vec![],
            default_rmw: "zenoh".into(),
            default_transport: "ethernet".into(),
            runner: "qemu".into(),
            prj_conf: "prj.conf".into(),
            board_conf: "x.conf".into(),
            board_overlay: "x.overlay".into(),
        };
        let mut cmake: BTreeMap<String, String> = BTreeMap::new();
        cmake.insert("NROS_BOARD_RUNNER".into(), "armfvp".into());
        let drift = compute_drift(&cargo, &cmake);
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0].field, "runner");
        assert_eq!(drift[0].cargo_metadata, "qemu");
        assert_eq!(drift[0].board_cmake, "armfvp");
    }
}
