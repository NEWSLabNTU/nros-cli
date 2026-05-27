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

use crate::orchestration::root_config::{DeployKind, DeployTarget, WorkspaceConfig};

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

    /// Resolve + print the steps without running them.
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
    deploy_target(&cfg, &root, &name, deploy, args.dry_run)
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
    dry_run: bool,
) -> Result<()> {
    cfg.system_for(deploy).ok_or_else(|| {
        eyre!("deploy '{name}': no resolvable [system] (set `system = \"<name>\"`)")
    })?;

    assert_pin(root, name, deploy)?;
    emit_entry_lib(name, deploy);

    // The `self` auto-build (generate entry lib + cargo a startup shim) is
    // WP-B. Until then a `self` target with no explicit steps can't produce a
    // binary; vendor-* targets always carry their `build[]` (enforced by
    // `validate`).
    if matches!(deploy.kind, DeployKind::Self_) && deploy.build.is_empty() {
        bail!(
            "deploy '{name}' (kind=self): the self build path is pending WP-B \
             (entry-lib emit + cargo). Provide explicit `build = [...]` steps, \
             or use a vendor-* kind."
        );
    }

    let vars = build_vars(root, name, deploy);
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

/// WP-B emits the wiring library here (compiled `.a`+header, or source). Until
/// that lands, announce the gap so build steps referencing `{entry_*}` are
/// understood to point at not-yet-generated paths.
fn emit_entry_lib(name: &str, deploy: &DeployTarget) {
    let form = deploy.emit.unwrap_or_else(|| deploy.kind.default_emit());
    eprintln!(
        "nros deploy: {name} entry-lib emit (form={form:?}) is pending WP-B — \
         steps using {{entry_lib}}/{{entry_src}}/{{entry_header}} reference \
         build/{name}/ paths not yet generated"
    );
}

type Vars = BTreeMap<&'static str, String>;

fn build_vars(root: &Path, name: &str, deploy: &DeployTarget) -> Vars {
    let mut v = Vars::new();

    // Entry-lib artifact paths under the deploy's build dir (WP-B produces them).
    let out = root.join("build").join(name);
    v.insert("entry_lib", out.join("libentry.a").display().to_string());
    v.insert("entry_src", out.join("entry").display().to_string());
    v.insert("entry_header", out.join("entry.h").display().to_string());

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
    v
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
        let v = build_vars(root, "mcu", &c.deploy["mcu"]);
        assert_eq!(v["board"], "nucleo_h753zi");
        assert_eq!(v["target"], "zephyr");
        assert_eq!(v["self"], "/ws/deploy/mcu");
        assert_eq!(v["entry_lib"], "/ws/build/mcu/libentry.a");
        assert!(!v.contains_key("vendor.dir")); // no vendor on this target
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
    fn self_with_no_build_is_pending_wp_b() {
        let c = cfg();
        let err = deploy_target(&c, Path::new("/ws"), "native", &c.deploy["native"], true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("pending WP-B"), "{err}");
    }

    #[test]
    fn dry_run_vendor_module_substitutes_without_running() {
        let c = cfg();
        // dry-run resolves + substitutes; no shell spawned.
        deploy_target(&c, Path::new("/ws"), "mcu", &c.deploy["mcu"], true).expect("dry-run ok");
    }
}
