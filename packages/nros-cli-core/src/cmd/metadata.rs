//! `nros metadata` - collect generated component source metadata.

use crate::orchestration::workspace::Workspace;
use clap::Args as ClapArgs;
use eyre::{Result, WrapErr, bail, eyre};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

/// Mirrors `nros::MISSING_COMPONENT_EXPORT_ERROR` (in
/// `packages/core/nros/src/component.rs`) so host-side diagnostics
/// surface the same human-readable phrase as the in-tree
/// `ComponentError::MissingExport` runtime variant. Held as a
/// `const` here to keep the CLI off the `nros` build dependency
/// closure (the latter is `no_std` + target-feature-gated).
pub(crate) const MISSING_COMPONENT_EXPORT_ERROR: &str =
    "package has no exported nros component";

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

    // Phase 126.B.7 acceptance — every package that declared itself
    // a nros component (via `component_nros.toml`) must have actually
    // produced its source-metadata JSON. A missing file is the host-
    // side surface for "forgot to write `nros::component!`": the
    // metadata-mode binary either failed to build or built but exited
    // before writing the JSON, both shapes leave the declared
    // `[metadata].source_metadata` path empty. Catch the case here
    // with the same diagnostic string the in-tree
    // `ComponentError::MissingExport` runtime variant uses.
    let declarations = workspace.component_declarations()?;
    let mut missing: Vec<(String, PathBuf)> = Vec::new();
    for decl in &declarations {
        let metadata_path = decl.source_metadata_path();
        if !metadata_path.is_file() {
            missing.push((decl.config.package.clone(), metadata_path));
        }
    }
    if !missing.is_empty() {
        let mut msg = String::from(MISSING_COMPONENT_EXPORT_ERROR);
        for (pkg, path) in &missing {
            msg.push_str(&format!(
                "\n  - package `{pkg}`: expected source metadata at {}",
                path.display()
            ));
        }
        msg.push_str(
            "\n  hint: add `nros::component!(YourComponent);` to the package's `lib.rs`/`main.rs` \
             and re-run the metadata build.",
        );
        bail!(msg);
    }

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
