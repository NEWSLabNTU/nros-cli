//! `nros codegen system` — Phase 212.E host-time system bake.
//!
//! Reads `<bringup>/system.toml` + `<bringup>/launch/system.launch.xml` and
//! emits the baked compile-time C config + component-registration glue that
//! every embedded RTOS adapter consumes (see
//! `docs/design/rtos-integration-pattern.md`).
//!
//! Outputs land under `<out>/nros-system/`:
//!
//! * `system_config.h` — `#define`s for domain, RMW, locator, QoS.
//! * `system_main.c`   — extern decls of `nros_component_<name>_register`
//!                       symbols, an entry `main()` that calls each in turn
//!                       and spins.
//! * `Cargo.toml`      — workspace stub for Rust components (only emitted if
//!                       at least one component lives in a Rust package).
//! * `nros-plan.json`  — the resolved plan (a thin host-side record of the
//!                       inputs the bake consumed; keeps `nros explain` /
//!                       `nros check` self-contained).
//!
//! Optional `--ahead-of-vendor <kind>` mode emits hookless-vendor artifacts:
//!
//! * `--ahead-of-vendor pio`  — `library.json` snippet next to the bake dir.
//! * `--ahead-of-vendor px4`  — one `<component>_module/` skeleton dir per
//!                              component matching PX4's `px4_add_module`
//!                              template (skeleton only; integration logic
//!                              deferred to Phase 212.H.7).

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use clap::{Args as ClapArgs, ValueEnum};
use eyre::{Context, Result, bail};
use serde::Serialize;

use crate::orchestration::{
    cargo_metadata_schema::{SystemComponentEntry, SystemToml},
    nros_config::{BringupPackageEntry, NrosConfig},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AheadOfVendor {
    /// Emit a PlatformIO `library.json` augment next to the bake dir.
    Pio,
    /// Emit one `<component>_module/` skeleton per component (PX4 shape).
    Px4,
}

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Workspace root (defaults to cwd).
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Bringup package name (or path to its directory). Defaults to the
    /// workspace's `[workspace.metadata.nros].default_system`.
    #[arg(long)]
    pub bringup: Option<String>,

    /// Target triple (for cross-compile bake context; recorded into the
    /// plan but doesn't drive codegen logic).
    #[arg(long)]
    pub target: Option<String>,

    /// Output directory (the `nros-system/` subdir is created inside this).
    /// Defaults to `<workspace>/build/<bringup>/`.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Hookless-vendor mode (`pio` or `px4`). Emits vendor-native artifacts
    /// in addition to the standard bake tree.
    #[arg(long = "ahead-of-vendor", value_enum)]
    pub ahead_of_vendor: Option<AheadOfVendor>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = match args.workspace {
        Some(p) => p,
        None => std::env::current_dir().context("resolve cwd")?,
    };

    let cfg = NrosConfig::from_cargo_metadata(&workspace)
        .with_context(|| format!("load workspace at {}", workspace.display()))?;

    let bringup = resolve_bringup(&cfg, args.bringup.as_deref())?;

    let out_dir = args
        .out
        .unwrap_or_else(|| workspace.join("build").join(&bringup.name));
    let bake_dir = out_dir.join("nros-system");

    let component_kinds = classify_components(&cfg, &bringup.system.components);

    emit_bake_tree(&bake_dir, bringup, &component_kinds, args.target.as_deref())?;

    if let Some(mode) = args.ahead_of_vendor {
        emit_ahead_of_vendor(&out_dir, bringup, mode)?;
    }

    eprintln!(
        "nros codegen system: wrote bake tree at {}",
        bake_dir.display()
    );
    Ok(())
}

/// Whether a component's host package is a Rust workspace member or
/// something else (C/C++ cmake pkg, unknown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComponentLang {
    Rust,
    Other,
}

/// Resolve `--bringup` (name or path) → a `BringupPackageEntry`. Falls back
/// to the workspace's `default_system` pointer when no explicit hint given.
fn resolve_bringup<'a>(
    cfg: &'a NrosConfig,
    hint: Option<&str>,
) -> Result<&'a BringupPackageEntry> {
    let name = match hint {
        Some(h) => {
            // Treat as path first: if it points at an existing dir whose
            // basename matches a registered bringup, prefer that.
            let as_path = PathBuf::from(h);
            if as_path.is_dir() {
                let base = as_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(h)
                    .to_string();
                if cfg.bringup_packages.contains_key(&base) {
                    base
                } else {
                    bail!(
                        "directory {h:?} does not match any bringup package \
                         in workspace; known bringup pkgs: {:?}",
                        cfg.bringup_packages.keys().collect::<Vec<_>>()
                    );
                }
            } else {
                h.to_string()
            }
        }
        None => cfg
            .workspace_metadata
            .default_system
            .clone()
            .ok_or_else(|| {
                eyre::eyre!(
                    "no --bringup hint and `[workspace.metadata.nros].default_system` \
                     is unset; supply `--bringup <name>`"
                )
            })?,
    };

    cfg.bringup_packages.get(&name).ok_or_else(|| {
        eyre::eyre!(
            "no bringup package `{name}` in workspace; known: {:?}",
            cfg.bringup_packages.keys().collect::<Vec<_>>()
        )
    })
}

/// For each component, decide whether its host package is a Rust workspace
/// member (so we should include it in the emitted Cargo.toml stub).
fn classify_components(
    cfg: &NrosConfig,
    components: &[SystemComponentEntry],
) -> Vec<(String, ComponentLang)> {
    components
        .iter()
        .map(|c| {
            let kind = if cfg.component_packages.contains_key(&c.pkg) {
                ComponentLang::Rust
            } else {
                ComponentLang::Other
            };
            (c.pkg.clone(), kind)
        })
        .collect()
}

/// Emit the standard `nros-system/` bake tree.
fn emit_bake_tree(
    bake_dir: &Path,
    bringup: &BringupPackageEntry,
    component_kinds: &[(String, ComponentLang)],
    target: Option<&str>,
) -> Result<()> {
    fs::create_dir_all(bake_dir)
        .with_context(|| format!("create bake dir {}", bake_dir.display()))?;

    write_if_changed(
        &bake_dir.join("system_config.h"),
        &render_system_config_h(&bringup.system),
    )?;
    write_if_changed(
        &bake_dir.join("system_main.c"),
        &render_system_main_c(&bringup.system),
    )?;

    let rust_pkgs: BTreeSet<&str> = component_kinds
        .iter()
        .filter(|(_, k)| *k == ComponentLang::Rust)
        .map(|(p, _)| p.as_str())
        .collect();
    if !rust_pkgs.is_empty() {
        write_if_changed(
            &bake_dir.join("Cargo.toml"),
            &render_cargo_workspace_stub(&rust_pkgs),
        )?;
    } else {
        // Idempotency: a previous run with Rust components may have left a
        // stale Cargo.toml; remove it so the directory matches the current
        // input.
        let stale = bake_dir.join("Cargo.toml");
        if stale.exists() {
            let _ = fs::remove_file(stale);
        }
    }

    write_if_changed(
        &bake_dir.join("nros-plan.json"),
        &render_plan_json(bringup, component_kinds, target)?,
    )?;

    Ok(())
}

/// Write `contents` to `path` only if the on-disk bytes differ (preserves
/// mtimes, satisfies the idempotency contract).
fn write_if_changed(path: &Path, contents: &str) -> Result<()> {
    if let Ok(existing) = fs::read_to_string(path) {
        if existing == contents {
            return Ok(());
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent {}", parent.display()))?;
    }
    let mut f = fs::File::create(path)
        .with_context(|| format!("create {}", path.display()))?;
    f.write_all(contents.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

fn render_system_config_h(sys: &SystemToml) -> String {
    let mut out = String::new();
    out.push_str("/* Auto-generated by `nros codegen system` — do not edit. */\n");
    out.push_str("\n");
    out.push_str("#ifndef NROS_SYSTEM_CONFIG_H\n");
    out.push_str("#define NROS_SYSTEM_CONFIG_H\n");
    out.push_str("\n");
    out.push_str(&format!(
        "#define NROS_SYSTEM_NAME \"{}\"\n",
        c_escape(&sys.system.name)
    ));
    out.push_str(&format!(
        "#define NROS_SYSTEM_DOMAIN_ID {}u\n",
        sys.system.domain_id
    ));
    out.push_str(&format!(
        "#define NROS_SYSTEM_RMW \"{}\"\n",
        c_escape(&sys.system.rmw)
    ));
    // Token form (`NROS_SYSTEM_RMW_<UPPER>`) is the form vendor adapters key
    // off (#ifdef tests against a known set, matching the per-RMW Kconfig
    // overlays).
    out.push_str(&format!(
        "#define NROS_SYSTEM_RMW_{}\n",
        sys.system.rmw.to_ascii_uppercase().replace('-', "_")
    ));
    if let Some(loc) = &sys.system.locator {
        out.push_str(&format!(
            "#define NROS_SYSTEM_LOCATOR \"{}\"\n",
            c_escape(loc)
        ));
    }
    out.push_str(&format!(
        "#define NROS_SYSTEM_COMPONENT_COUNT {}\n",
        sys.components.len()
    ));
    out.push_str("\n");
    for (idx, c) in sys.components.iter().enumerate() {
        out.push_str(&format!(
            "#define NROS_SYSTEM_COMPONENT_{}_NAME \"{}\"\n",
            idx,
            c_escape(&c.name)
        ));
        out.push_str(&format!(
            "#define NROS_SYSTEM_COMPONENT_{}_PKG \"{}\"\n",
            idx,
            c_escape(&c.pkg)
        ));
        out.push_str(&format!(
            "#define NROS_SYSTEM_COMPONENT_{}_CLASS \"{}\"\n",
            idx,
            c_escape(&c.class)
        ));
    }
    // QoS placeholder — until the planner lowers QoS overrides into the
    // SystemToml, the bake emits a sentinel macro so adapters can detect the
    // absence rather than guess.
    out.push_str("\n");
    out.push_str("#define NROS_SYSTEM_QOS_DEFAULT 1\n");
    out.push_str("\n");
    out.push_str("#endif /* NROS_SYSTEM_CONFIG_H */\n");
    out
}

fn render_system_main_c(sys: &SystemToml) -> String {
    let mut out = String::new();
    out.push_str("/* Auto-generated by `nros codegen system` — do not edit. */\n");
    out.push_str("\n");
    out.push_str("#include \"system_config.h\"\n");
    out.push_str("\n");
    out.push_str("/* Forward declarations of per-component register hooks. */\n");
    for c in &sys.components {
        out.push_str(&format!(
            "extern int nros_component_{}_register(void);\n",
            c_ident(&c.name)
        ));
    }
    out.push_str("\n");
    out.push_str("/* Implemented by the linked nano-ros runtime. */\n");
    out.push_str("extern int  nros_system_init(void);\n");
    out.push_str("extern void nros_system_spin(void);\n");
    out.push_str("\n");
    out.push_str("int nros_system_main(void) {\n");
    out.push_str("    int rc = nros_system_init();\n");
    out.push_str("    if (rc != 0) { return rc; }\n");
    for c in &sys.components {
        out.push_str(&format!(
            "    rc = nros_component_{}_register();\n",
            c_ident(&c.name)
        ));
        out.push_str("    if (rc != 0) { return rc; }\n");
    }
    out.push_str("    nros_system_spin();\n");
    out.push_str("    return 0;\n");
    out.push_str("}\n");
    out
}

fn render_cargo_workspace_stub(rust_pkgs: &BTreeSet<&str>) -> String {
    let mut out = String::new();
    out.push_str("# Auto-generated by `nros codegen system` — do not edit.\n");
    out.push_str("[workspace]\n");
    out.push_str("resolver = \"2\"\n");
    out.push_str("members = [\n");
    for p in rust_pkgs {
        out.push_str(&format!("    \"{p}\",\n"));
    }
    out.push_str("]\n");
    out
}

#[derive(Serialize)]
struct PlanComponent<'a> {
    name: &'a str,
    pkg: &'a str,
    class: &'a str,
    lang: &'a str,
}

#[derive(Serialize)]
struct PlanDoc<'a> {
    bringup: &'a str,
    system: &'a str,
    rmw: &'a str,
    domain_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    locator: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    launch_file: Option<&'a str>,
    components: Vec<PlanComponent<'a>>,
}

fn render_plan_json(
    bringup: &BringupPackageEntry,
    component_kinds: &[(String, ComponentLang)],
    target: Option<&str>,
) -> Result<String> {
    let launch_file: Option<String> = bringup
        .system
        .deploy
        .values()
        .find_map(|d| d.launch.clone())
        .or_else(|| {
            // Fall back to the conventional path.
            let candidate = bringup
                .manifest_path
                .parent()
                .map(|p| p.join("launch").join("system.launch.xml"));
            candidate.and_then(|c| c.exists().then(|| c.to_string_lossy().into_owned()))
        });

    let components: Vec<PlanComponent> = bringup
        .system
        .components
        .iter()
        .zip(component_kinds.iter())
        .map(|(c, (_, kind))| PlanComponent {
            name: &c.name,
            pkg: &c.pkg,
            class: &c.class,
            lang: match kind {
                ComponentLang::Rust => "rust",
                ComponentLang::Other => "other",
            },
        })
        .collect();

    let doc = PlanDoc {
        bringup: &bringup.name,
        system: &bringup.system.system.name,
        rmw: &bringup.system.system.rmw,
        domain_id: bringup.system.system.domain_id,
        locator: bringup.system.system.locator.as_deref(),
        target,
        launch_file: launch_file.as_deref(),
        components,
    };
    let mut s =
        serde_json::to_string_pretty(&doc).context("serialize plan json")?;
    s.push('\n');
    Ok(s)
}

// ---------------------------------------------------------------------------
// Ahead-of-vendor emit
// ---------------------------------------------------------------------------

fn emit_ahead_of_vendor(
    out_dir: &Path,
    bringup: &BringupPackageEntry,
    mode: AheadOfVendor,
) -> Result<()> {
    match mode {
        AheadOfVendor::Pio => emit_pio(out_dir, bringup),
        AheadOfVendor::Px4 => emit_px4(out_dir, bringup),
    }
}

fn emit_pio(out_dir: &Path, bringup: &BringupPackageEntry) -> Result<()> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("create {}", out_dir.display()))?;
    // Minimal `library.json` snippet pointing at the staticlib build tree.
    // Full PIO integration (extra_script.py, transport selection) is
    // deferred to Phase 212.H.6; this emits the manifest skeleton only.
    let body = format!(
        "{{\n  \"name\": \"{}\",\n  \"version\": \"0.0.0\",\n  \
         \"description\": \"nano-ros bake for {} (auto-generated)\",\n  \
         \"build\": {{\n    \"srcDir\": \"nros-system\"\n  }}\n}}\n",
        json_escape(&bringup.name),
        json_escape(&bringup.system.system.name),
    );
    write_if_changed(&out_dir.join("library.json"), &body)?;
    Ok(())
}

fn emit_px4(out_dir: &Path, bringup: &BringupPackageEntry) -> Result<()> {
    // PX4 expects one module dir per `px4_add_module` call. Phase 212.E
    // emits a skeleton (CMakeLists.txt + module.h with TODO markers); the
    // full `px4_add_module` integration is deferred to Phase 212.H.7.
    for c in &bringup.system.components {
        let mod_dir = out_dir.join(format!("{}_module", c_ident(&c.name)));
        fs::create_dir_all(&mod_dir)
            .with_context(|| format!("create {}", mod_dir.display()))?;
        let cmakelists = format!(
            "# Auto-generated skeleton for PX4 component `{}`.\n\
             # TODO(212.H.7): fill in `px4_add_module(...)` invocation.\n\
             # Component class: {}\n\
             # Source pkg:      {}\n",
            c.name, c.class, c.pkg
        );
        write_if_changed(&mod_dir.join("CMakeLists.txt"), &cmakelists)?;
        let module_h = format!(
            "/* Auto-generated skeleton for PX4 component `{}`.\n\
             * TODO(212.H.7): bridge to uORB + register with nano-ros runtime.\n\
             */\n",
            c.name
        );
        write_if_changed(&mod_dir.join("module.h"), &module_h)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Escape a string for use inside a C double-quoted string literal.
fn c_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Lower a component name to a valid C identifier (replace non-alnum with `_`).
fn c_ident(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    // Avoid leading digit.
    if out.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(test: &str) -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("nros-cli-core-tests"));
        let dir = base.join(format!("codegen_system_{test}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Write a "zephyr_native_sim" style fixture: 2 Rust components + bringup.
    fn write_rust_two_component_workspace(dir: &Path) {
        fs::write(
            dir.join("Cargo.toml"),
            r#"
[workspace]
resolver = "2"
members = ["talker_pkg", "listener_pkg", "demo_bringup"]

[workspace.metadata.nros]
default_system = "demo_bringup"
"#,
        )
        .unwrap();

        for pkg in ["talker_pkg", "listener_pkg"] {
            fs::create_dir_all(dir.join(pkg).join("src")).unwrap();
            fs::write(
                dir.join(pkg).join("Cargo.toml"),
                format!(
                    r#"
[package]
name = "{pkg}"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[package.metadata.nros.component]
default_namespace = "/demo"
"#
                ),
            )
            .unwrap();
            fs::write(dir.join(pkg).join("src/lib.rs"), "").unwrap();
        }

        fs::create_dir_all(dir.join("demo_bringup/launch")).unwrap();
        fs::create_dir_all(dir.join("demo_bringup/src")).unwrap();
        fs::write(
            dir.join("demo_bringup/Cargo.toml"),
            r#"
[package]
name = "demo_bringup"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
        )
        .unwrap();
        fs::write(dir.join("demo_bringup/src/lib.rs"), "").unwrap();
        fs::write(
            dir.join("demo_bringup/system.toml"),
            r#"
[system]
name = "demo"
rmw = "zenoh"
domain_id = 7
locator = "tcp/127.0.0.1:7447"

[[component]]
pkg = "talker_pkg"
class = "talker_pkg::TalkerNode"
name = "talker"

[[component]]
pkg = "listener_pkg"
class = "listener_pkg::ListenerNode"
name = "listener"

[deploy.zephyr_native_sim]
kind = "qemu"
target = "x86_64-unknown-linux-gnu"
board = "native_sim"
launch = "launch/system.launch.xml"
"#,
        )
        .unwrap();
        fs::write(
            dir.join("demo_bringup/launch/system.launch.xml"),
            "<launch></launch>\n",
        )
        .unwrap();
    }

    /// Workspace whose components live in non-Rust (C/C++) packages — i.e.
    /// the bringup names `pkg = "..."` entries that aren't registered in the
    /// cargo workspace's `component_packages`.
    fn write_pure_cpp_workspace(dir: &Path) {
        fs::write(
            dir.join("Cargo.toml"),
            r#"
[workspace]
resolver = "2"
members = ["demo_bringup"]

[workspace.metadata.nros]
default_system = "demo_bringup"
"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("demo_bringup/src")).unwrap();
        fs::write(
            dir.join("demo_bringup/Cargo.toml"),
            r#"
[package]
name = "demo_bringup"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
        )
        .unwrap();
        fs::write(dir.join("demo_bringup/src/lib.rs"), "").unwrap();
        fs::write(
            dir.join("demo_bringup/system.toml"),
            r#"
[system]
name = "demo"
rmw = "cyclonedds"
domain_id = 0

[[component]]
pkg = "cpp_talker_pkg"
class = "cpp_talker_pkg::Talker"
name = "talker"
"#,
        )
        .unwrap();
    }

    /// 212.E.T1 — fixture bringup w/ 2 Rust components produces the expected
    /// baked tree under `<out>/nros-system/`.
    #[test]
    fn codegen_system_emits_baked_headers_for_zephyr_native_sim() {
        let dir = scratch_dir("emits_baked_headers_for_zephyr_native_sim");
        write_rust_two_component_workspace(&dir);

        let out = dir.join("build/demo_bringup");
        run(Args {
            workspace: Some(dir.clone()),
            bringup: None,
            target: Some("x86_64-unknown-linux-gnu".into()),
            out: Some(out.clone()),
            ahead_of_vendor: None,
        })
        .expect("codegen runs");

        let bake = out.join("nros-system");
        let header = fs::read_to_string(bake.join("system_config.h")).unwrap();
        assert!(header.contains("#define NROS_SYSTEM_DOMAIN_ID 7u"), "header: {header}");
        assert!(header.contains("#define NROS_SYSTEM_RMW \"zenoh\""), "header: {header}");
        assert!(header.contains("#define NROS_SYSTEM_RMW_ZENOH"), "header: {header}");
        assert!(header.contains("#define NROS_SYSTEM_LOCATOR \"tcp/127.0.0.1:7447\""));
        assert!(header.contains("#define NROS_SYSTEM_COMPONENT_COUNT 2"));
        assert!(header.contains("#define NROS_SYSTEM_COMPONENT_0_NAME \"talker\""));
        assert!(header.contains("#define NROS_SYSTEM_COMPONENT_1_NAME \"listener\""));

        let main_c = fs::read_to_string(bake.join("system_main.c")).unwrap();
        assert!(main_c.contains("extern int nros_component_talker_register(void);"));
        assert!(main_c.contains("extern int nros_component_listener_register(void);"));
        assert!(main_c.contains("nros_component_talker_register();"));
        assert!(main_c.contains("nros_component_listener_register();"));
        assert!(main_c.contains("nros_system_spin();"));

        let cargo_stub = fs::read_to_string(bake.join("Cargo.toml")).unwrap();
        assert!(cargo_stub.contains("\"talker_pkg\""));
        assert!(cargo_stub.contains("\"listener_pkg\""));

        let plan = fs::read_to_string(bake.join("nros-plan.json")).unwrap();
        assert!(plan.contains("\"bringup\": \"demo_bringup\""));
        assert!(plan.contains("\"system\": \"demo\""));
        assert!(plan.contains("\"target\": \"x86_64-unknown-linux-gnu\""));
        assert!(plan.contains("\"lang\": \"rust\""));
        // Launch file path recorded from the deploy block.
        assert!(plan.contains("launch/system.launch.xml"));
    }

    /// 212.E.T2 — re-running with identical inputs produces byte-identical
    /// outputs across all emitted files.
    #[test]
    fn codegen_system_idempotent_on_unchanged_input() {
        let dir = scratch_dir("idempotent_on_unchanged_input");
        write_rust_two_component_workspace(&dir);

        let out = dir.join("build/demo_bringup");
        let args = || Args {
            workspace: Some(dir.clone()),
            bringup: None,
            target: Some("x86_64-unknown-linux-gnu".into()),
            out: Some(out.clone()),
            ahead_of_vendor: None,
        };
        run(args()).expect("first run");

        let bake = out.join("nros-system");
        let snap: Vec<(String, Vec<u8>)> = ["system_config.h", "system_main.c", "Cargo.toml", "nros-plan.json"]
            .iter()
            .map(|f| (f.to_string(), fs::read(bake.join(f)).expect("read")))
            .collect();

        run(args()).expect("second run");

        for (name, before) in snap {
            let after = fs::read(bake.join(&name)).expect("read");
            assert_eq!(before, after, "file `{name}` changed across runs");
        }
    }

    /// 212.E.T3 — bringup whose components live entirely outside the cargo
    /// workspace (i.e. C/C++ pkgs) → no Cargo.toml stub emitted.
    #[test]
    fn codegen_system_emits_only_for_rust_components_when_no_rust() {
        let dir = scratch_dir("emits_only_for_rust_when_no_rust");
        write_pure_cpp_workspace(&dir);

        let out = dir.join("build/demo_bringup");
        run(Args {
            workspace: Some(dir.clone()),
            bringup: None,
            target: None,
            out: Some(out.clone()),
            ahead_of_vendor: None,
        })
        .expect("codegen runs");

        let bake = out.join("nros-system");
        assert!(bake.join("system_config.h").exists());
        assert!(bake.join("system_main.c").exists());
        assert!(!bake.join("Cargo.toml").exists(), "no Rust components → no Cargo stub");
        assert!(bake.join("nros-plan.json").exists());

        let plan = fs::read_to_string(bake.join("nros-plan.json")).unwrap();
        assert!(plan.contains("\"lang\": \"other\""), "non-Rust comp tagged: {plan}");
    }

    /// 212.E.T4 — `--ahead-of-vendor pio` mode emits `library.json` alongside
    /// the standard bake tree.
    #[test]
    fn codegen_system_ahead_of_vendor_emits_pio_library_json() {
        let dir = scratch_dir("ahead_of_vendor_pio_library_json");
        write_rust_two_component_workspace(&dir);

        let out = dir.join("build/demo_bringup");
        run(Args {
            workspace: Some(dir.clone()),
            bringup: None,
            target: None,
            out: Some(out.clone()),
            ahead_of_vendor: Some(AheadOfVendor::Pio),
        })
        .expect("codegen runs");

        let lib = out.join("library.json");
        assert!(lib.exists(), "library.json at {}", lib.display());
        let body = fs::read_to_string(&lib).unwrap();
        assert!(body.contains("\"name\": \"demo_bringup\""), "body: {body}");
        assert!(body.contains("\"srcDir\": \"nros-system\""), "body: {body}");
        // Standard bake still produced.
        assert!(out.join("nros-system/system_config.h").exists());
    }
}
