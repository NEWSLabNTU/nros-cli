//! `nros plan` - generate host-side orchestration plan.
//!
//! Phase 212.L.6: the positional `<launch_file>` now also accepts a
//! **package directory** (Cargo / CMake pkg, or a bringup pkg). When a
//! directory is passed we route through
//! [`orchestration::launch_synth::resolve_launch`] which either picks a
//! convention-named launch file under `<dir>/launch/` or synthesises a
//! one-node `<launch>` body in-memory for self-bringup pkgs.

use crate::orchestration::{
    launch_synth::resolve_launch,
    planner::{PlanOptions, plan_system},
};
use clap::Args as ClapArgs;
use eyre::Result;
use std::path::PathBuf;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// System package name used for build/<system_pkg>/nros output
    pub system_pkg: String,

    /// ROS 2 launch file to parse, **or** a package directory to resolve
    /// via the Phase 212.L.6 multi-launch policy (pkg-named →
    /// system.launch.xml → single-file → synth for self-bringup pkgs).
    pub launch_file: PathBuf,

    /// Precomputed play_launch record.json to use instead of parsing launch_file
    #[arg(long)]
    pub record: Option<PathBuf>,

    /// Phase 212.L.6 — when `<launch_file>` is a directory, prefer
    /// `<dir>/launch/<file>` (or cwd-relative / absolute as fallback).
    #[arg(long = "file")]
    pub file: Option<String>,

    /// Phase 212.L.6 — disambiguates the synthesised `<node exec="…">`
    /// when the package declares multiple `[[bin]]` / `add_executable`
    /// targets.
    #[arg(long = "exec")]
    pub exec: Option<String>,

    /// Workspace root containing colcon-like src/* packages
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Output root for orchestration artifacts
    #[arg(long)]
    pub out_dir: Option<PathBuf>,

    /// Existing source metadata JSON artifact
    #[arg(long = "metadata")]
    pub metadata: Vec<PathBuf>,

    /// ROS launch manifest YAML artifact
    #[arg(long = "manifest")]
    pub manifests: Vec<PathBuf>,

    /// nano-ros deployment overlay TOML
    #[arg(long = "nros-toml")]
    pub nros_toml: Vec<PathBuf>,

    /// Launch arguments forwarded as name:=value or name=value
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub launch_args: Vec<String>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace_root = args.workspace.unwrap_or(std::env::current_dir()?);
    let out_root = args.out_dir.unwrap_or_else(|| {
        workspace_root
            .join("build")
            .join(&args.system_pkg)
            .join("nros")
    });

    // Phase 212.L.6: the positional `launch_file` may be either an
    // existing file (legacy path) or a package directory. Resolve to a
    // real on-disk path that the external `play_launch_parser` binary
    // can consume — synthesised XML is written to a temp file whose
    // lifetime is tied to `_materialised` and removed when planning
    // returns.
    let (resolved_path, _materialised) = if args.launch_file.is_dir() {
        let input = resolve_launch(
            &args.launch_file,
            args.file.as_deref(),
            args.exec.as_deref(),
        )?;
        let materialised = input.materialise()?;
        (materialised.path.clone(), Some(materialised))
    } else {
        (args.launch_file.clone(), None)
    };

    let output = plan_system(PlanOptions {
        system_pkg: args.system_pkg,
        workspace_root,
        launch_file: resolved_path,
        record_file: args.record,
        out_root,
        metadata_files: args.metadata,
        manifest_files: args.manifests,
        nros_toml_files: args.nros_toml,
        launch_args: args.launch_args,
    })?;

    // `_materialised` keeps the synthesised temp file alive through
    // `plan_system`; drop it now (RAII removes the temp file).
    drop(_materialised);

    eprintln!(
        "nros plan: wrote {} and {}",
        output.record_path.display(),
        output.plan_path.display()
    );
    Ok(())
}
