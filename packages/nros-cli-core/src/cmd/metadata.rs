//! `nros metadata` - collect generated component source metadata.

use crate::orchestration::workspace::Workspace;
use clap::Args as ClapArgs;
use eyre::{Result, WrapErr, eyre};
use serde_json::Value;
use std::{fs, path::PathBuf};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// System package name used for build/<system_pkg>/nros output
    pub system_pkg: String,

    /// Workspace root containing colcon-like src/* packages
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Output root for orchestration artifacts
    #[arg(long)]
    pub out_dir: Option<PathBuf>,

    /// Existing source metadata JSON to validate and preserve
    #[arg(long = "metadata")]
    pub metadata: Vec<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let root = args.workspace.unwrap_or(std::env::current_dir()?);
    let out_root = args
        .out_dir
        .unwrap_or_else(|| root.join("build").join(&args.system_pkg).join("nros"));
    let metadata_dir = out_root.join("metadata");
    fs::create_dir_all(&metadata_dir)?;

    let workspace = Workspace::discover(&root)?;
    let mut inputs = args.metadata;
    if inputs.is_empty() {
        inputs.extend(workspace.source_metadata_files());
    }

    for path in &inputs {
        let raw = fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read source metadata {}", path.display()))?;
        let _: Value = serde_json::from_str(&raw)
            .wrap_err_with(|| format!("invalid source metadata JSON {}", path.display()))?;
        let file_name = path
            .file_name()
            .ok_or_else(|| eyre!("metadata path has no file name: {}", path.display()))?;
        fs::write(metadata_dir.join(file_name), raw)?;
    }

    eprintln!(
        "nros metadata: preserved {} metadata artifact(s) in {}",
        inputs.len(),
        metadata_dir.display()
    );
    Ok(())
}
