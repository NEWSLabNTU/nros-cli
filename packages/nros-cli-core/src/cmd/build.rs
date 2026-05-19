//! `nros build` — Phase 111.A.9.
//!
//! Two paths, both auto-detected from the argument shape:
//!
//! 1. **One-shot orchestration** (Phase 126.M4): when `--launch` is
//!    supplied, run `metadata` → `plan` → generation → cargo build in
//!    sequence against a colcon-like workspace. Requires `--system-pkg`
//!    (the orchestration system name). Pre-existing
//!    metadata/manifest artifacts may be supplied via `--metadata` /
//!    `--manifest` to skip re-collection.
//!
//! 2. **Pre-planned generation**: when `--system-plan` is supplied,
//!    skip metadata + plan and just generate + cargo build from the
//!    existing `nros-plan.json` (the long-standing pre-126.M4 path).
//!
//! Otherwise auto-detect the project flavor and delegate (Zephyr west,
//! CMake, or Cargo) — detection precedence (highest first), evaluated
//! in the project root (cwd or `--project`):
//!
//!   1. `prj.conf` present → Zephyr → `west build`
//!   2. `CMakeLists.txt` present + no `Cargo.toml` → `cmake -B build && cmake --build build`
//!   3. `Cargo.toml` present → `cargo build`
//!
//! Mixed projects (Cargo.toml AND CMakeLists.txt) — common when a Rust
//! crate produces a `staticlib` consumed by C/C++ — go through the
//! cmake path. Heuristic: if `[lib].crate-type` in Cargo.toml contains
//! `staticlib` AND CMakeLists.txt exists, prefer cmake.

use crate::cmd::{metadata, plan};
use crate::orchestration;
use clap::Args as ClapArgs;
use eyre::{Result, WrapErr, bail, eyre};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the project root (default: cwd)
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Build a generated nano-ros system package from this nros-plan.json
    /// (skips the metadata + plan steps; expects a pre-generated plan).
    #[arg(long)]
    pub system_plan: Option<PathBuf>,

    /// One-shot orchestration mode (Phase 126.M4): run `metadata` +
    /// `plan` + generation + cargo build in sequence from a ROS 2
    /// launch file. Requires `--system-pkg`. Mutually exclusive with
    /// `--system-plan`.
    #[arg(long, conflicts_with = "system_plan")]
    pub launch: Option<PathBuf>,

    /// Orchestration system package name (one-shot mode). Mirrors
    /// `nros metadata <system_pkg>` + `nros plan <system_pkg> ...`.
    #[arg(long)]
    pub system_pkg: Option<String>,

    /// Pre-collected source metadata JSON (one-shot mode). Repeatable.
    /// Same shape as `nros metadata --metadata`.
    #[arg(long = "metadata")]
    pub metadata: Vec<PathBuf>,

    /// Launch-manifest YAML files (one-shot mode). Repeatable. Same
    /// shape as `nros plan --manifests`.
    #[arg(long = "manifest")]
    pub manifest: Vec<PathBuf>,

    /// Launch arguments passed through to `nros plan` (one-shot mode).
    /// Repeatable. Mirrors `nros plan --launch-arg`.
    #[arg(long = "launch-arg")]
    pub launch_arg: Vec<String>,

    /// Output root for orchestration artifacts (one-shot mode).
    /// Default: `<project>/build/<system_pkg>/nros`. Mirrors the
    /// `--out-dir` flag on `nros metadata` / `nros plan`.
    #[arg(long)]
    pub out_dir: Option<PathBuf>,

    /// Output dir for the generated package (default: <out_dir>/generated)
    #[arg(long)]
    pub system_output: Option<PathBuf>,

    /// Generated package name for system mode
    #[arg(long)]
    pub system_package: Option<String>,

    /// nano-ros workspace root for generated path dependencies
    #[arg(long)]
    pub nano_ros_workspace: Option<PathBuf>,

    /// Build generated system package in release mode
    #[arg(long)]
    pub release: bool,

    /// Cargo target triple for generated system package
    #[arg(long)]
    pub target: Option<String>,

    /// Trailing arguments forwarded verbatim to the underlying tool
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub passthrough: Vec<String>,
}

pub fn run(args: Args) -> Result<()> {
    let root = match args.project.clone() {
        Some(p) => p,
        None => std::env::current_dir()?,
    };

    // Phase 126.M4 — one-shot orchestration mode.
    if let Some(launch_file) = args.launch.clone() {
        let system_pkg = args.system_pkg.clone().ok_or_else(|| {
            eyre!(
                "`nros build --launch` requires `--system-pkg <name>` \
                 (the orchestration system package name; mirrors \
                 `nros metadata <name>`)"
            )
        })?;
        let out_root = args
            .out_dir
            .clone()
            .unwrap_or_else(|| root.join("build").join(&system_pkg).join("nros"));
        let generated_dir = args
            .system_output
            .clone()
            .unwrap_or_else(|| out_root.join("generated"));
        let workspace_root = args.nano_ros_workspace.clone().unwrap_or_else(|| root.clone());
        let package_name = args
            .system_package
            .clone()
            .unwrap_or_else(|| infer_package_name(&root));

        metadata::run(metadata::Args {
            system_pkg: system_pkg.clone(),
            workspace: Some(root.clone()),
            out_dir: Some(out_root.clone()),
            metadata: args.metadata.clone(),
        })
        .wrap_err("metadata step (one-shot build) failed")?;

        plan::run(plan::Args {
            system_pkg: system_pkg.clone(),
            launch_file,
            record: None,
            workspace: Some(root.clone()),
            out_dir: Some(out_root.clone()),
            metadata: Vec::new(),
            manifests: args.manifest.clone(),
            nros_toml: Vec::new(),
            launch_args: args.launch_arg.clone(),
        })
        .wrap_err("plan step (one-shot build) failed")?;

        let plan_path = out_root.join("nros-plan.json");
        if !plan_path.is_file() {
            bail!(
                "plan step produced no nros-plan.json at {}",
                plan_path.display()
            );
        }

        orchestration::build::build_generated_package(&orchestration::build::BuildOptions {
            package_name,
            output_dir: generated_dir,
            plan_path,
            workspace_root,
            component_workspace: Some(root.clone()),
            release: args.release,
            target: args.target,
            cargo_args: args.passthrough,
        })?;
        return Ok(());
    }

    if let Some(plan_path) = args.system_plan {
        let package_name = args
            .system_package
            .unwrap_or_else(|| infer_package_name(&root));
        let output_dir = args.system_output.unwrap_or_else(|| {
            root.join("build")
                .join(&package_name)
                .join("nros/generated")
        });
        let workspace_root = args.nano_ros_workspace.unwrap_or_else(|| root.clone());
        orchestration::build::build_generated_package(&orchestration::build::BuildOptions {
            package_name,
            output_dir,
            plan_path,
            workspace_root,
            component_workspace: Some(root.clone()),
            release: args.release,
            target: args.target,
            cargo_args: args.passthrough,
        })?;
        return Ok(());
    }

    let flavor = detect_flavor(&root)?;
    eprintln!("nros build: flavor = {flavor:?} ({})", root.display());

    let mut cmd = match flavor {
        Flavor::West => {
            let mut c = Command::new("west");
            c.arg("build");
            c
        }
        Flavor::Cmake => {
            // `cmake -B build && cmake --build build` chained as one
            // shell, but we keep them as two child processes so we don't
            // need a shell.
            let configure = Command::new("cmake")
                .current_dir(&root)
                .args(["-B", "build", "-S", "."])
                .args(&args.passthrough)
                .status()
                .wrap_err("failed to invoke `cmake -B build`")?;
            if !configure.success() {
                return Err(eyre!(
                    "cmake configure failed (exit {})",
                    configure.code().unwrap_or(-1)
                ));
            }
            let mut c = Command::new("cmake");
            c.arg("--build").arg("build");
            c
        }
        Flavor::Cargo => {
            let mut c = Command::new("cargo");
            c.arg("build");
            c
        }
    };
    if !matches!(flavor, Flavor::Cmake) {
        cmd.args(&args.passthrough);
    }
    cmd.current_dir(&root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = cmd
        .status()
        .wrap_err_with(|| format!("failed to invoke build for {flavor:?}"))?;
    if !status.success() {
        return Err(eyre!("build failed (exit {})", status.code().unwrap_or(-1)));
    }
    Ok(())
}

fn infer_package_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_package_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "nros-system".to_string())
}

fn sanitize_package_name(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[derive(Debug)]
enum Flavor {
    West,
    Cmake,
    Cargo,
}

fn detect_flavor(root: &Path) -> Result<Flavor> {
    let has_prj_conf = root.join("prj.conf").is_file();
    let has_cmake = root.join("CMakeLists.txt").is_file();
    let cargo_toml = root.join("Cargo.toml");
    let has_cargo = cargo_toml.is_file();

    if has_prj_conf {
        return Ok(Flavor::West);
    }

    if has_cmake && has_cargo && produces_staticlib(&cargo_toml).unwrap_or(false) {
        return Ok(Flavor::Cmake);
    }
    if has_cargo {
        return Ok(Flavor::Cargo);
    }
    if has_cmake {
        return Ok(Flavor::Cmake);
    }
    Err(eyre!(
        "no build flavor detected at {}: expected prj.conf (Zephyr), \
         CMakeLists.txt (CMake), or Cargo.toml (Rust)",
        root.display()
    ))
}

fn produces_staticlib(cargo_toml: &Path) -> Result<bool> {
    let raw = fs::read_to_string(cargo_toml)?;
    let doc: toml::Value = toml::from_str(&raw)?;
    let Some(lib) = doc.get("lib") else {
        return Ok(false);
    };
    let Some(crate_type) = lib.get("crate-type").or_else(|| lib.get("crate_type")) else {
        return Ok(false);
    };
    Ok(match crate_type {
        toml::Value::Array(arr) => arr.iter().any(|v| v.as_str() == Some("staticlib")),
        toml::Value::String(s) => s == "staticlib",
        _ => false,
    })
}
