//! `nros deploy` — Phase 172 WP-A command-runner.
//!
//! Runs a `[deploy.<name>]` target from the root `nros.toml`: assert the
//! vendor pin → emit the entry-lib form (WP-B; stubbed) → run `build[]` →
//! `package[]`, substituting `{self}` / `{entry_lib}` / `{entry_src}` /
//! `{entry_header}` / `{board}` / `{target}` / `{vendor.dir}` into each shell
//! step. No per-vendor code lives here — the vendor knowledge is the
//! user-authored `build[]` / `package[]` lines.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use clap::Args as ClapArgs;
use eyre::{Result, WrapErr, bail, eyre};

use crate::{
    cmd::{metadata, plan},
    orchestration::{
        self,
        generate::{GenerateOptions, generate_package},
        root_config::{DeployTarget, EmitForm, SystemSection, WorkspaceConfig},
    },
};

/// Tokens the runner substitutes in `build[]` / `package[]` steps. A `{token}`
/// for one of these that the target can't resolve is an error; any other
/// `{...}` is left verbatim (shell brace syntax).
const KNOWN_VARS: &[&str] = &[
    "self",
    "entry_lib",
    "entry_src",
    "entry_header",
    "board",
    "target",
    "vendor.dir",
];

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Deploy target name (`[deploy.<name>]`); omit to use `[workspace].default`.
    pub name: Option<String>,

    /// Root nros.toml
    #[arg(long, default_value = "nros.toml")]
    pub config: PathBuf,

    /// nano-ros workspace root (for the generated entry lib's path deps).
    /// Falls back to the `NROS_WORKSPACE` env var.
    #[arg(long)]
    pub nano_ros_workspace: Option<PathBuf>,

    /// Resolve + print the steps without generating/building or running them.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let cfg = WorkspaceConfig::load(&args.config)?;
    let root = args
        .config
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let (name, deploy) = resolve(&cfg, args.name.as_deref())?;
    let nano_ros = args
        .nano_ros_workspace
        .clone()
        .or_else(|| std::env::var_os("NROS_WORKSPACE").map(PathBuf::from));
    deploy_target(
        &cfg,
        &root,
        &name,
        deploy,
        nano_ros.as_deref(),
        args.dry_run,
    )
}

/// Resolve the target by name, or fall back to `[workspace].default`.
fn resolve<'c>(cfg: &'c WorkspaceConfig, name: Option<&str>) -> Result<(String, &'c DeployTarget)> {
    match name {
        Some(n) => cfg
            .deploy
            .get(n)
            .map(|d| (n.to_string(), d))
            .ok_or_else(|| eyre!("no [deploy.{n}] in the root nros.toml")),
        None => cfg
            .default_deploy()
            .map(|(n, d)| (n.clone(), d))
            .ok_or_else(|| {
                eyre!("no deploy name given and no [workspace].default set in the root nros.toml")
            }),
    }
}

fn deploy_target(
    cfg: &WorkspaceConfig,
    root: &Path,
    name: &str,
    deploy: &DeployTarget,
    nano_ros: Option<&Path>,
    dry_run: bool,
) -> Result<()> {
    let sys = cfg.system_for(deploy).ok_or_else(|| {
        eyre!("deploy '{name}': no resolvable [system] (set `system = \"<name>\"`)")
    })?;

    assert_pin(root, name, deploy)?;

    // Real run: drive metadata → plan → entry-lib emit (WP-B). `--dry-run`
    // skips the heavy pipeline and just resolves + prints the steps.
    if !dry_run {
        let nano_ros = nano_ros.ok_or_else(|| {
            eyre!(
                "deploy '{name}': need the nano-ros workspace — pass \
                 --nano-ros-workspace <path> or set NROS_WORKSPACE"
            )
        })?;
        emit_entry_lib(root, name, sys, deploy, nano_ros)?;
    }

    let vars = build_vars(root, name, deploy)?;
    run_phase("build", &deploy.build, &vars, root, dry_run)?;
    run_phase("package", &deploy.package, &vars, root, dry_run)?;

    if !dry_run {
        eprintln!("nros deploy: {name} complete");
    }
    Ok(())
}

/// Drift guard: when a vendor pin is declared, the vendor dir must resolve +
/// exist. The exact version compare is vendor-specific (WP-C per-vendor);
/// here we assert presence and surface the expected pin.
fn assert_pin(root: &Path, name: &str, deploy: &DeployTarget) -> Result<()> {
    let Some(vendor) = &deploy.vendor else {
        return Ok(());
    };
    let Some(pin) = &vendor.pin else {
        return Ok(());
    };
    let dir = vendor.dir.resolve().map(|d| abs(root, &d)).ok_or_else(|| {
        eyre!(
            "deploy '{name}': vendor pinned at '{pin}' but its dir is unset \
                 (set the env var or a default)"
        )
    })?;
    if !dir.exists() {
        bail!(
            "deploy '{name}': vendor dir {} not found (pin '{pin}' expects it) — \
             install the SDK or set the env var",
            dir.display()
        );
    }
    eprintln!(
        "nros deploy: {name} vendor pinned '{pin}' at {}",
        dir.display()
    );
    Ok(())
}

/// Generated-artifact layout for a deploy target. Pure path computation
/// (deterministic from the target triple + name), so it works in `--dry-run`
/// before anything is generated and matches what `emit_entry_lib` produces.
struct EntryPaths {
    /// Output root: `<root>/build/<name>/nros`.
    out_root: PathBuf,
    /// Generated entry crate (`{entry_src}`).
    src_dir: PathBuf,
    /// cbindgen C header (`{entry_header}`).
    header: PathBuf,
    /// Compiled staticlib (`{entry_lib}`); exists only after a compiled-form build.
    lib: PathBuf,
}

fn entry_paths(root: &Path, name: &str, deploy: &DeployTarget) -> Result<EntryPaths> {
    let triple = deploy.target.as_deref().ok_or_else(|| {
        eyre!("deploy '{name}': set `target = \"<triple>\"` (needed for the entry lib)")
    })?;
    let out_root = root.join("build").join(name).join("nros");
    let src_dir = out_root.join("generated");
    let pkg = package_name(name);
    Ok(EntryPaths {
        header: src_dir
            .join("include")
            .join(format!("{}.h", system_ident(name))),
        lib: out_root
            .join("target")
            .join(triple)
            .join("debug")
            .join(format!("lib{}.a", pkg.replace('-', "_"))),
        src_dir,
        out_root,
    })
}

/// Generated package name → drives the staticlib name (`lib<pkg>.a`).
fn package_name(name: &str) -> String {
    format!("nros-{name}")
}

/// System identifier baked into the header name (the `nros plan` system pkg,
/// which is the deploy name here).
fn system_ident(name: &str) -> String {
    name.replace('-', "_")
}

/// Drive metadata → plan → entry-lib emission (WP-B). Compiled-form kinds
/// (`self` / `vendor-lib`) build the staticlib (+ self-shim binary); the
/// source form (`vendor-module`) only generates the crate + CMake fragment for
/// the vendor toolchain to compile.
fn emit_entry_lib(
    root: &Path,
    name: &str,
    sys: &SystemSection,
    deploy: &DeployTarget,
    nano_ros: &Path,
) -> Result<()> {
    let launch = sys.launch.as_ref().ok_or_else(|| {
        eyre!("deploy '{name}': [system].launch is required for a planned deploy")
    })?;
    let paths = entry_paths(root, name, deploy)?;
    let system_pkg = name.to_string();

    std::fs::create_dir_all(&paths.out_root)
        .wrap_err_with(|| format!("create {}", paths.out_root.display()))?;
    let overlay = synth_build_overlay(&paths.out_root, sys, deploy)?;

    metadata::run(metadata::Args {
        system_pkg: system_pkg.clone(),
        workspace: Some(root.to_path_buf()),
        out_dir: Some(paths.out_root.clone()),
        metadata: Vec::new(),
        // A real deploy produces any missing component source-metadata via the
        // metadata-mode build (172.E driver), using the same nano-ros workspace.
        build: true,
        nano_ros_workspace: Some(nano_ros.to_path_buf()),
    })
    .wrap_err("deploy: metadata step")?;

    plan::run(plan::Args {
        system_pkg: system_pkg.clone(),
        launch_file: root.join(launch),
        record: None,
        workspace: Some(root.to_path_buf()),
        out_dir: Some(paths.out_root.clone()),
        metadata: Vec::new(),
        manifests: Vec::new(),
        nros_toml: vec![overlay],
        launch_args: Vec::new(),
    })
    .wrap_err("deploy: plan step")?;

    let plan_path = paths.out_root.join("nros-plan.json");
    let emit = deploy.emit.unwrap_or_else(|| deploy.kind.default_emit());
    match emit {
        EmitForm::Source => {
            generate_package(&GenerateOptions {
                package_name: package_name(name),
                output_dir: paths.src_dir.clone(),
                plan_path,
                nros_path: nano_ros.join("packages/core/nros"),
                nros_orchestration_path: nano_ros.join("packages/core/nros-orchestration"),
                component_workspace: Some(root.to_path_buf()),
            })
            .wrap_err("deploy: generate entry-lib source")?;
            eprintln!(
                "nros deploy: {name} entry-lib source at {}",
                paths.src_dir.display()
            );
        }
        EmitForm::Compiled => {
            orchestration::build::build_generated_package(&orchestration::build::BuildOptions {
                package_name: package_name(name),
                output_dir: paths.src_dir.clone(),
                plan_path,
                workspace_root: nano_ros.to_path_buf(),
                component_workspace: Some(root.to_path_buf()),
                release: false,
                target: deploy.target.clone(),
                cargo_args: Vec::new(),
                force: false,
            })
            .wrap_err("deploy: build entry lib")?;
            eprintln!(
                "nros deploy: {name} entry-lib staticlib at {}",
                paths.lib.display()
            );
        }
    }
    Ok(())
}

/// Bridge the root `[system]`/`[deploy]` into the planner's `[build]` overlay
/// (target/board/rmw) so the generated entry lib lowers the right RMW. Written
/// into the deploy's build dir (ephemeral).
fn synth_build_overlay(
    out_root: &Path,
    sys: &SystemSection,
    deploy: &DeployTarget,
) -> Result<PathBuf> {
    let target = deploy
        .target
        .as_deref()
        .unwrap_or("x86_64-unknown-linux-gnu");
    let board = deploy.board.as_deref().unwrap_or("native");
    let rmw = deploy
        .rmw
        .as_deref()
        .or(sys.rmw.as_deref())
        .unwrap_or("zenoh");
    let body = format!("[build]\ntarget = \"{target}\"\nboard = \"{board}\"\nrmw = \"{rmw}\"\n");
    let path = out_root.join("deploy-build-overlay.toml");
    std::fs::write(&path, body).wrap_err_with(|| format!("write {}", path.display()))?;
    Ok(path)
}

type Vars = BTreeMap<&'static str, String>;

fn build_vars(root: &Path, name: &str, deploy: &DeployTarget) -> Result<Vars> {
    let mut v = Vars::new();

    // Real entry-lib artifact paths (match what `emit_entry_lib` produces).
    let paths = entry_paths(root, name, deploy)?;
    v.insert("entry_lib", paths.lib.display().to_string());
    v.insert("entry_src", paths.src_dir.display().to_string());
    v.insert("entry_header", paths.header.display().to_string());

    if let Some(self_dir) = &deploy.self_dir {
        v.insert("self", abs(root, Path::new(self_dir)).display().to_string());
    }
    if let Some(board) = &deploy.board {
        v.insert("board", board.clone());
    }
    if let Some(target) = &deploy.target {
        v.insert("target", target.clone());
    }
    if let Some(vendor) = &deploy.vendor
        && let Some(dir) = vendor.dir.resolve()
    {
        v.insert("vendor.dir", abs(root, &dir).display().to_string());
    }
    Ok(v)
}

/// Substitute the known `{token}`s. A referenced-but-undefined known token is
/// an error (catches a target that forgot `self`/`board`/…); unknown `{...}`
/// is left for the shell.
fn substitute(template: &str, vars: &Vars) -> Result<String> {
    for tok in KNOWN_VARS {
        if template.contains(&format!("{{{tok}}}")) && !vars.contains_key(*tok) {
            bail!("deploy step references {{{tok}}} but this target doesn't define it");
        }
    }
    let mut out = template.to_string();
    for (k, val) in vars {
        out = out.replace(&format!("{{{k}}}"), val);
    }
    Ok(out)
}

fn run_phase(phase: &str, steps: &[String], vars: &Vars, root: &Path, dry_run: bool) -> Result<()> {
    let n = steps.len();
    for (i, step) in steps.iter().enumerate() {
        let cmd = substitute(step, vars)?;
        if dry_run {
            println!("[{phase} {}/{n}] {cmd}", i + 1);
            continue;
        }
        eprintln!("nros deploy: [{phase} {}/{n}] {cmd}", i + 1);
        let status = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(root)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .wrap_err_with(|| format!("spawn {phase} step: {cmd}"))?;
        if !status.success() {
            bail!(
                "{phase} step {}/{n} failed (exit {}): {cmd}",
                i + 1,
                status.code().unwrap_or(-1)
            );
        }
    }
    Ok(())
}

/// A relative path is resolved against the workspace root; an absolute path is
/// kept as-is.
fn abs(root: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::root_config::{VendorDir, VendorSpec};

    fn cfg() -> WorkspaceConfig {
        toml::from_str(
            r#"
[workspace]
default = "native"
[system]
rmw = "zenoh"
[deploy.native]
target = "x86_64-unknown-linux-gnu"
[deploy.mcu]
kind = "vendor-module"
target = "zephyr"
board = "nucleo_h753zi"
self = "deploy/mcu"
build = ["west build -b {board} -d build/mcu {self}"]
"#,
        )
        .expect("parse")
    }

    #[test]
    fn resolve_by_name_default_and_missing() {
        let c = cfg();
        assert_eq!(resolve(&c, Some("mcu")).unwrap().0, "mcu");
        assert_eq!(resolve(&c, None).unwrap().0, "native"); // [workspace].default
        assert!(resolve(&c, Some("ghost")).is_err());
    }

    #[test]
    fn resolve_no_default_errors() {
        let c: WorkspaceConfig = toml::from_str("[system]\nrmw=\"zenoh\"\n").unwrap();
        assert!(resolve(&c, None).is_err());
    }

    #[test]
    fn build_vars_resolves_self_board_target_and_entry_paths() {
        let c = cfg();
        let root = Path::new("/ws");
        let v = build_vars(root, "mcu", &c.deploy["mcu"]).expect("vars");
        assert_eq!(v["board"], "nucleo_h753zi");
        assert_eq!(v["target"], "zephyr");
        assert_eq!(v["self"], "/ws/deploy/mcu");
        // Real entry-lib paths under build/<name>/nros (match emit_entry_lib).
        assert_eq!(v["entry_src"], "/ws/build/mcu/nros/generated");
        assert_eq!(
            v["entry_header"],
            "/ws/build/mcu/nros/generated/include/mcu.h"
        );
        assert_eq!(
            v["entry_lib"],
            "/ws/build/mcu/nros/target/zephyr/debug/libnros_mcu.a"
        );
        assert!(!v.contains_key("vendor.dir")); // no vendor on this target
    }

    #[test]
    fn build_vars_needs_a_target() {
        let c: WorkspaceConfig =
            toml::from_str("[system]\nrmw=\"zenoh\"\n[deploy.x]\nkind=\"self\"\n").unwrap();
        assert!(build_vars(Path::new("/ws"), "x", &c.deploy["x"]).is_err());
    }

    #[test]
    fn synth_build_overlay_carries_target_board_rmw_override() {
        let c = cfg();
        let dir = std::env::temp_dir();
        let p = synth_build_overlay(&dir, c.system.as_ref().unwrap(), &c.deploy["mcu"]).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("target = \"zephyr\""));
        assert!(body.contains("board = \"nucleo_h753zi\""));
        // mcu has no rmw override → falls back to [system].rmw.
        assert!(body.contains("rmw = \"zenoh\""));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn substitute_replaces_known_and_keeps_unknown_braces() {
        let mut v = Vars::new();
        v.insert("board", "b1".to_string());
        v.insert("self", "/ws/deploy/x".to_string());
        let out = substitute("west build -b {board} {self} -exec {}", &v).unwrap();
        assert_eq!(out, "west build -b b1 /ws/deploy/x -exec {}");
    }

    #[test]
    fn substitute_errors_on_referenced_undefined_known_var() {
        let v = Vars::new(); // nothing defined
        let err = substitute("link {entry_lib}", &v).unwrap_err().to_string();
        assert!(err.contains("{entry_lib}"), "{err}");
    }

    #[test]
    fn assert_pin_errors_when_vendor_dir_absent() {
        let mut d = cfg().deploy["mcu"].clone();
        d.vendor = Some(VendorSpec {
            dir: VendorDir::Path("/definitely/missing/sdk".to_string()),
            pin: Some("sdk 1.0".to_string()),
        });
        let err = assert_pin(Path::new("/ws"), "mcu", &d)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn dry_run_skips_pipeline_and_needs_no_nano_ros() {
        let c = cfg();
        // --dry-run resolves + substitutes; no metadata/plan/build, no shell,
        // and no nano-ros workspace required.
        deploy_target(
            &c,
            Path::new("/ws"),
            "native",
            &c.deploy["native"],
            None,
            true,
        )
        .expect("self dry-run ok");
        deploy_target(&c, Path::new("/ws"), "mcu", &c.deploy["mcu"], None, true)
            .expect("vendor-module dry-run ok");
    }

    #[test]
    fn real_run_without_nano_ros_errors() {
        let c = cfg();
        let err = deploy_target(
            &c,
            Path::new("/ws"),
            "native",
            &c.deploy["native"],
            None,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("nano-ros workspace"), "{err}");
    }
}
