//! Phase 212.F — `nros new system <name>_bringup --components <list>` scaffolder.
//!
//! Materializes a **Path A** bringup package per the multi-node workspace
//! layout design (`docs/design/multi-node-workspace-layout.md` §4 +
//! `docs/design/workspace-layout-by-case.md` Case 3). A bringup package is
//! **pure declarative** — `package.xml` + `system.toml` + `launch/` only. No
//! `Cargo.toml`, no `CMakeLists.txt`, no `src/`.
//!
//! When invoked inside an existing cargo workspace, the bringup pkg name is
//! appended to the workspace-root `[workspace] exclude` list (Path A keeps
//! bringup out of `members`).
//!
//! Surface (see `cmd/new.rs` dispatcher):
//!
//! ```text
//! nros new system <name>_bringup --components <pkg1,pkg2,...> [--workspace-root <dir>] [--force]
//! ```
//!
//! Each `<pkgN>` becomes one `[[component]]` entry in `system.toml`, one
//! `<exec_depend>` line in `package.xml`, and one `<node>` block in
//! `launch/system.launch.xml`. Component crates themselves are **NOT**
//! scaffolded — that's `nros new --component <name>` (Phase 172 W.3).

use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::Args as ClapArgs;
use eyre::{Result, WrapErr, bail};
use toml_edit::{Array, DocumentMut, value};

use crate::orchestration::cargo_metadata_schema::{
    SystemComponentEntry, SystemHeader, SystemToml,
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Bringup package directory to create (e.g. `demo_bringup`). Conventionally
    /// suffixed `_bringup` per the Phase 212 design doc, but any name is
    /// accepted (`<system>_launch` documented as an alias).
    pub name: PathBuf,

    /// Comma-separated component package names to wire into the bringup
    /// (`pkg1,pkg2,…`). One `[[component]]` entry + one `<exec_depend>` +
    /// one launch `<node>` per name.
    #[arg(long, value_delimiter = ',', required = true)]
    pub components: Vec<String>,

    /// Workspace root holding the cargo workspace `Cargo.toml` we should
    /// update. Defaults to the current dir's nearest workspace root (parent
    /// of the bringup dir for an absolute path, else cwd). When no workspace
    /// `Cargo.toml` exists we still scaffold the bringup pkg — only the
    /// `[workspace] exclude` update is skipped.
    #[arg(long)]
    pub workspace_root: Option<PathBuf>,

    /// Overwrite an existing bringup directory.
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: Args) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let bringup_dir = if args.name.is_absolute() {
        args.name.clone()
    } else {
        cwd.join(&args.name)
    };

    let pkg_name = bringup_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| eyre::eyre!("invalid bringup package name"))?
        .to_string();

    // Workspace-root resolution: explicit flag → parent of bringup dir → cwd.
    let workspace_root = args
        .workspace_root
        .clone()
        .or_else(|| bringup_dir.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cwd.clone());

    let _ = scaffold_bringup(&BringupScaffold {
        bringup_dir,
        pkg_name,
        components: args.components,
        workspace_root,
        force: args.force,
    })?;
    Ok(())
}

/// Resolved inputs for [`scaffold_bringup`] — public so tests + the
/// dispatcher (`cmd/new.rs`) can build it without going through clap.
pub struct BringupScaffold {
    pub bringup_dir: PathBuf,
    pub pkg_name: String,
    pub components: Vec<String>,
    pub workspace_root: PathBuf,
    pub force: bool,
}

/// Result of a successful scaffold — paths written, for tests + UX.
#[derive(Debug)]
pub struct ScaffoldedBringup {
    pub bringup_dir: PathBuf,
    pub package_xml: PathBuf,
    pub system_toml: PathBuf,
    pub launch_file: PathBuf,
    pub gitignore: PathBuf,
    /// `Some(path)` when a workspace `Cargo.toml` existed and got updated.
    pub workspace_cargo_toml: Option<PathBuf>,
}

pub fn scaffold_bringup(s: &BringupScaffold) -> Result<ScaffoldedBringup> {
    if s.components.is_empty() {
        bail!("at least one --components <pkg> is required");
    }
    for c in &s.components {
        if c.trim().is_empty() {
            bail!("empty component name in --components list");
        }
    }

    if s.bringup_dir.exists() {
        if !s.force {
            bail!(
                "bringup directory already exists at {} — pass --force to overwrite",
                s.bringup_dir.display()
            );
        }
        fs::remove_dir_all(&s.bringup_dir).wrap_err_with(|| {
            format!("remove existing {} for --force", s.bringup_dir.display())
        })?;
    }

    fs::create_dir_all(s.bringup_dir.join("launch"))
        .wrap_err_with(|| format!("create {}", s.bringup_dir.display()))?;

    let package_xml = s.bringup_dir.join("package.xml");
    fs::write(&package_xml, render_package_xml(&s.pkg_name, &s.components))
        .wrap_err_with(|| format!("write {}", package_xml.display()))?;

    let system_toml = s.bringup_dir.join("system.toml");
    fs::write(&system_toml, render_system_toml(&s.pkg_name, &s.components)?)
        .wrap_err_with(|| format!("write {}", system_toml.display()))?;

    let launch_file = s.bringup_dir.join("launch").join("system.launch.xml");
    fs::write(
        &launch_file,
        render_launch_xml(&s.pkg_name, &s.components),
    )
    .wrap_err_with(|| format!("write {}", launch_file.display()))?;

    let gitignore = s.bringup_dir.join(".gitignore");
    fs::write(&gitignore, "/target/\n/build/\n")
        .wrap_err_with(|| format!("write {}", gitignore.display()))?;

    let workspace_cargo_toml =
        add_bringup_to_workspace_exclude(&s.workspace_root, &s.pkg_name)?;

    Ok(ScaffoldedBringup {
        bringup_dir: s.bringup_dir.clone(),
        package_xml,
        system_toml,
        launch_file,
        gitignore,
        workspace_cargo_toml,
    })
}

fn render_package_xml(pkg_name: &str, components: &[String]) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\"?>\n");
    s.push_str(
        "<package format=\"3\">\n\
         <!-- generated by nros new system — Phase 212.F -->\n",
    );
    s.push_str(&format!("  <name>{pkg_name}</name>\n"));
    s.push_str("  <version>0.1.0</version>\n");
    s.push_str(&format!(
        "  <description>Bringup package for {pkg_name} (declarative system spec).</description>\n"
    ));
    s.push_str("  <maintainer email=\"nobody@example.invalid\">TODO maintainer</maintainer>\n");
    s.push_str("  <license>Apache-2.0</license>\n");
    for c in components {
        s.push_str(&format!("  <exec_depend>{c}</exec_depend>\n"));
    }
    s.push_str("  <export>\n");
    s.push_str("    <build_type>ament_nros</build_type>\n");
    s.push_str("  </export>\n");
    s.push_str("</package>\n");
    s
}

fn render_system_toml(pkg_name: &str, components: &[String]) -> Result<String> {
    // The pkg_name conventionally ends in `_bringup`; the system name strips
    // that suffix (`demo_bringup` → `demo`). When the convention is broken we
    // fall back to the full name.
    let system_name = pkg_name
        .strip_suffix("_bringup")
        .or_else(|| pkg_name.strip_suffix("_launch"))
        .unwrap_or(pkg_name)
        .to_string();

    let entries: Vec<SystemComponentEntry> = components
        .iter()
        .map(|pkg| SystemComponentEntry {
            pkg: pkg.clone(),
            // Placeholder — user fills in real Rust path / C++ class. Documented
            // as TODO in the file header below.
            class: format!("{pkg}::TODO"),
            // Default the node name to the package name; the user typically
            // edits it to drop the `_pkg` suffix.
            name: pkg.clone(),
        })
        .collect();

    let model = SystemToml {
        system: SystemHeader {
            name: system_name,
            rmw: "zenoh".to_string(),
            domain_id: 0,
            locator: None,
        },
        components: entries,
        deploy: Default::default(),
        domains: Vec::new(),
        bridges: Vec::new(),
    };

    let body = toml::to_string_pretty(&model)
        .wrap_err("serialize generated system.toml")?;

    let mut out = String::new();
    out.push_str("# generated by nros new system — Phase 212.F\n");
    out.push_str("# TODO: fill in real component `class` paths (Rust module-path / C++ class).\n");
    out.push_str("# See docs/design/workspace-layout-by-case.md §3 for the full schema.\n");
    out.push_str("\n");
    out.push_str(&body);
    Ok(out)
}

fn render_launch_xml(pkg_name: &str, components: &[String]) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\"?>\n");
    s.push_str(&format!(
        "<!-- generated by nros new system — bringup pkg {pkg_name}, Phase 212.F -->\n"
    ));
    s.push_str("<launch>\n");
    for c in components {
        // node name = pkg name (user edits later); exec = pkg name. Matches
        // the `cmake` Case 4 example in workspace-layout-by-case.md.
        s.push_str(&format!(
            "  <node pkg=\"{c}\" exec=\"{c}\" name=\"{c}\" />\n"
        ));
    }
    s.push_str("</launch>\n");
    s
}

/// Append `pkg_name` to the workspace-root `Cargo.toml`'s `[workspace] exclude`
/// list (Path A). Returns `None` when no workspace `Cargo.toml` lives at the
/// given root (still allowed — the bringup pkg works fine outside a cargo
/// workspace, e.g. for pure-C++ users).
fn add_bringup_to_workspace_exclude(
    workspace_root: &Path,
    pkg_name: &str,
) -> Result<Option<PathBuf>> {
    let cargo_toml = workspace_root.join("Cargo.toml");
    if !cargo_toml.is_file() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&cargo_toml)
        .wrap_err_with(|| format!("read {}", cargo_toml.display()))?;
    let mut doc: DocumentMut = raw
        .parse()
        .wrap_err_with(|| format!("parse {}", cargo_toml.display()))?;

    // Ensure `[workspace]` table.
    let workspace = doc
        .entry("workspace")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| eyre::eyre!("[workspace] is not a table in {}", cargo_toml.display()))?;

    // Read existing exclude list (default empty).
    let mut exclude_array = workspace
        .get("exclude")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_else(Array::new);

    let already = exclude_array.iter().any(|v| v.as_str() == Some(pkg_name));
    if !already {
        exclude_array.push(pkg_name);
    }
    workspace["exclude"] = value(exclude_array);

    fs::write(&cargo_toml, doc.to_string())
        .wrap_err_with(|| format!("write {}", cargo_toml.display()))?;

    Ok(Some(cargo_toml))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "nros-new-system-{tag}-{}-{stamp}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_workspace_cargo_toml(root: &Path) {
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"talker_pkg\", \"listener_pkg\"]\n",
        )
        .unwrap();
    }

    #[test]
    fn nros_new_system_scaffolds_bringup_pkg_with_expected_files() {
        let root = temp_root("scaffold_files");
        write_workspace_cargo_toml(&root);

        let out = scaffold_bringup(&BringupScaffold {
            bringup_dir: root.join("demo_bringup"),
            pkg_name: "demo_bringup".to_string(),
            components: vec!["talker_pkg".to_string(), "listener_pkg".to_string()],
            workspace_root: root.clone(),
            force: false,
        })
        .expect("scaffold succeeds");

        // 1. Expected tree exists.
        assert!(out.package_xml.is_file(), "package.xml: {:?}", out.package_xml);
        assert!(out.system_toml.is_file(), "system.toml: {:?}", out.system_toml);
        assert!(out.launch_file.is_file(), "launch xml: {:?}", out.launch_file);
        assert!(out.gitignore.is_file(), ".gitignore: {:?}", out.gitignore);

        // 2. NO forbidden files.
        assert!(!out.bringup_dir.join("Cargo.toml").exists());
        assert!(!out.bringup_dir.join("CMakeLists.txt").exists());
        assert!(!out.bringup_dir.join("src").exists());

        // 3. package.xml carries one <exec_depend> per component.
        let pkg_xml = fs::read_to_string(&out.package_xml).unwrap();
        assert!(pkg_xml.contains("<name>demo_bringup</name>"));
        assert!(pkg_xml.contains("<exec_depend>talker_pkg</exec_depend>"));
        assert!(pkg_xml.contains("<exec_depend>listener_pkg</exec_depend>"));

        // 4. system.toml round-trips through SystemToml.
        let sys: SystemToml =
            toml::from_str(&fs::read_to_string(&out.system_toml).unwrap()).unwrap();
        assert_eq!(sys.system.name, "demo");
        assert_eq!(sys.system.rmw, "zenoh");
        assert_eq!(sys.system.domain_id, 0);
        assert_eq!(sys.components.len(), 2);
        assert_eq!(sys.components[0].pkg, "talker_pkg");
        assert_eq!(sys.components[1].pkg, "listener_pkg");

        // 5. .gitignore is the documented two-liner.
        let gi = fs::read_to_string(&out.gitignore).unwrap();
        assert!(gi.contains("/target/"));
        assert!(gi.contains("/build/"));
    }

    #[test]
    fn nros_new_system_adds_to_workspace_exclude() {
        let root = temp_root("workspace_exclude");
        write_workspace_cargo_toml(&root);

        let out = scaffold_bringup(&BringupScaffold {
            bringup_dir: root.join("demo_bringup"),
            pkg_name: "demo_bringup".to_string(),
            components: vec!["talker_pkg".to_string()],
            workspace_root: root.clone(),
            force: false,
        })
        .unwrap();

        let cargo_toml_path = out
            .workspace_cargo_toml
            .as_ref()
            .expect("workspace Cargo.toml exists");
        let after = fs::read_to_string(cargo_toml_path).unwrap();
        let doc: toml::Value = toml::from_str(&after).unwrap();
        let exclude = doc
            .get("workspace")
            .and_then(|w| w.get("exclude"))
            .and_then(|e| e.as_array())
            .expect("[workspace] exclude is an array");
        let names: Vec<&str> = exclude.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            names.contains(&"demo_bringup"),
            "exclude must contain demo_bringup, got: {names:?}"
        );

        // Path A guard: bringup NOT in [workspace] members.
        let members = doc
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(|m| m.as_array())
            .expect("members present");
        let member_names: Vec<&str> = members.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            !member_names.contains(&"demo_bringup"),
            "Path A means bringup must NOT be in [workspace] members"
        );
    }

    #[test]
    fn nros_new_system_with_two_components_generates_two_node_blocks_in_launch_xml() {
        let root = temp_root("two_node_blocks");
        write_workspace_cargo_toml(&root);

        let out = scaffold_bringup(&BringupScaffold {
            bringup_dir: root.join("robot_bringup"),
            pkg_name: "robot_bringup".to_string(),
            components: vec!["talker_pkg".to_string(), "listener_pkg".to_string()],
            workspace_root: root.clone(),
            force: false,
        })
        .unwrap();

        let xml = fs::read_to_string(&out.launch_file).unwrap();
        let count = xml.matches("<node ").count();
        assert_eq!(count, 2, "expected 2 <node> blocks, got {count}:\n{xml}");
        assert!(xml.contains("pkg=\"talker_pkg\""));
        assert!(xml.contains("pkg=\"listener_pkg\""));
        assert!(xml.contains("<launch>") && xml.contains("</launch>"));
    }

    #[test]
    fn nros_new_system_without_workspace_cargo_toml_still_works() {
        let root = temp_root("no_workspace");
        // intentionally no Cargo.toml in root
        let out = scaffold_bringup(&BringupScaffold {
            bringup_dir: root.join("demo_bringup"),
            pkg_name: "demo_bringup".to_string(),
            components: vec!["talker_pkg".to_string()],
            workspace_root: root.clone(),
            force: false,
        })
        .unwrap();
        assert!(out.workspace_cargo_toml.is_none());
        assert!(out.package_xml.is_file());
    }

    #[test]
    fn nros_new_system_rejects_existing_dir_without_force() {
        let root = temp_root("no_force");
        let dir = root.join("demo_bringup");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("STAY"), "pre-existing").unwrap();
        let err = scaffold_bringup(&BringupScaffold {
            bringup_dir: dir,
            pkg_name: "demo_bringup".to_string(),
            components: vec!["talker_pkg".to_string()],
            workspace_root: root,
            force: false,
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "diagnostic: {err}"
        );
    }
}
