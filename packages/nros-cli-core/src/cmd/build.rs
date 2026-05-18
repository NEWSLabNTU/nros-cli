//! `nros build` — Phase 111.A.9.
//!
//! Auto-detect the project flavor and delegate. Detection precedence
//! (highest first), evaluated in the project root (cwd or `--project`):
//!
//!   1. `prj.conf` present → Zephyr → `west build`
//!   2. `CMakeLists.txt` present + no `Cargo.toml` → `cmake -B build && cmake --build build`
//!   3. `Cargo.toml` present → `cargo build`
//!
//! Mixed projects (Cargo.toml AND CMakeLists.txt) — common when a Rust
//! crate produces a `staticlib` consumed by C/C++ — go through the
//! cmake path. Heuristic: if `[lib].crate-type` in Cargo.toml contains
//! `staticlib` AND CMakeLists.txt exists, prefer cmake.

use crate::{
    cmd::{check, metadata, plan},
    orchestration,
};
use clap::Args as ClapArgs;
use eyre::{Result, WrapErr, eyre};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, Default, ClapArgs)]
pub struct Args {
    /// Path to the project root (default: cwd)
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Build a generated nano-ros system package from this nros-plan.json
    #[arg(long)]
    pub system_plan: Option<PathBuf>,

    /// Output dir for the generated package (default: <out_dir>/generated)
    #[arg(long)]
    pub system_output: Option<PathBuf>,

    /// Orchestration intermediate output root (default: <project>/build/<system_pkg>/nros)
    #[arg(long)]
    pub out_dir: Option<PathBuf>,

    /// Generated package Cargo crate name (default: nros-<system_pkg>-generated)
    #[arg(long)]
    pub system_package: Option<String>,

    /// Orchestration system package name used for build/<system_pkg>/nros layout
    #[arg(long)]
    pub system_pkg: Option<String>,

    /// Launch file driving the full metadata → plan → check → build chain
    #[arg(long)]
    pub launch_file: Option<PathBuf>,

    /// Precomputed play_launch record.json (skip launch parse step)
    #[arg(long)]
    pub record: Option<PathBuf>,

    /// Pre-existing source metadata JSON (repeatable; default = workspace auto-discover)
    #[arg(long = "metadata")]
    pub metadata: Vec<PathBuf>,

    /// ROS launch manifest YAML artifact (repeatable)
    #[arg(long = "manifest")]
    pub manifest: Vec<PathBuf>,

    /// nano-ros deployment overlay TOML (repeatable)
    #[arg(long = "nros-toml")]
    pub nros_toml: Vec<PathBuf>,

    /// Launch arguments forwarded as name:=value or name=value (repeatable)
    #[arg(long = "launch-arg")]
    pub launch_arg: Vec<String>,

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
    let root = match args.project {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    if args.launch_file.is_some() && args.system_plan.is_some() {
        return Err(eyre!(
            "`--launch-file` and `--system-plan` are mutually exclusive"
        ));
    }
    if let Some(launch_file) = args.launch_file {
        let system_pkg = args
            .system_pkg
            .clone()
            .or_else(|| args.system_package.clone())
            .unwrap_or_else(|| infer_system_pkg(&root));
        let package_name = args
            .system_package
            .clone()
            .unwrap_or_else(|| default_generated_crate_name(&system_pkg));
        let out_dir = args
            .out_dir
            .clone()
            .unwrap_or_else(|| root.join("build").join(&system_pkg).join("nros"));
        let generated_dir = args
            .system_output
            .clone()
            .unwrap_or_else(|| out_dir.join("generated"));
        fs::create_dir_all(&out_dir)
            .wrap_err_with(|| format!("failed to create out dir {}", out_dir.display()))?;
        metadata::run(metadata::Args {
            system_pkg: system_pkg.clone(),
            workspace: Some(root.clone()),
            out_dir: Some(out_dir.clone()),
            metadata: args.metadata.clone(),
        })?;
        plan::run(plan::Args {
            system_pkg: system_pkg.clone(),
            launch_file,
            record: args.record,
            workspace: Some(root.clone()),
            out_dir: Some(out_dir.clone()),
            metadata: Vec::new(),
            manifests: args.manifest,
            nros_toml: args.nros_toml,
            launch_args: args.launch_arg,
        })?;
        let plan_path = out_dir.join("nros-plan.json");
        check::run(check::Args {
            plan: plan_path.clone(),
        })?;
        let workspace_root = args.nano_ros_workspace.unwrap_or_else(|| root.clone());
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

fn infer_system_pkg(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .map(|raw| {
            raw.chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        ch
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "nros_system".to_string())
}

fn default_generated_crate_name(system_pkg: &str) -> String {
    let sanitized = sanitize_package_name(system_pkg);
    if sanitized.starts_with("nros-") || sanitized.starts_with("nros_") {
        sanitized
    } else {
        format!("nros-{sanitized}-generated")
    }
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
