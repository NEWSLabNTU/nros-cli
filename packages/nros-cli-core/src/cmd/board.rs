//! `nros board list` — Phase 111.A.8.
//!
//! Enumerate every `nros-board-*` crate under `<workspace>/packages/boards/`.
//!
//! Structured chip / flash / ram fields are deferred to UX-42 (board
//! descriptor TOML, out of Phase 111 scope per the phase doc). For now
//! we surface name + description, which is enough for users to pick the
//! right board crate on first contact.

use clap::{Args as ClapArgs, Subcommand};
use eyre::{Result, WrapErr, eyre};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Subcommand)]
pub enum Args {
    /// List every supported board crate
    List(ListArgs),
}

#[derive(Debug, ClapArgs)]
pub struct ListArgs {
    /// Path to the nano-ros workspace root (auto-detected by walking
    /// upward from cwd if omitted)
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    match args {
        Args::List(args) => list(args),
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

/// Walk upward from cwd until a directory containing `packages/boards/`
/// is found. Errors if none reached before the filesystem root.
pub(crate) fn find_workspace_root() -> Result<PathBuf> {
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
