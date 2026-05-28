//! Workspace and package discovery for host planning.

use cargo_nano_ros::package_xml::PackageXml;
use eyre::{Context, Result};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use super::config::ComponentConfig;

/// Permissive envelope for extracting a `[component]` table out of a package's
/// `nros.toml` (Phase 172 W.1 fold) while ignoring sibling tables
/// (`[workspace]` / `[system]` / `[deploy]` / `[node]` / `[[transport]]`).
/// Unknown keys are ignored on purpose — only `[component]` is read here.
#[derive(Debug, Deserialize)]
struct ComponentEnvelope {
    #[serde(default)]
    component: Option<ComponentConfig>,
}

/// Load a component declaration from a manifest path. Handles two forms:
///
/// - **Folded** (Phase 172 W.1): a `[component]` table inside a package's
///   `nros.toml`. Returns `Ok(None)` when that file carries no `[component]`
///   (it is a workspace-root / direct-mode manifest, not a component).
/// - **Legacy**: the standalone whole-file form (`component_nros.toml` or
///   `nros/components/*.toml`), which is deprecated and warns once per file.
pub fn load_component_config(path: &Path) -> Result<Option<ComponentConfig>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read component manifest {}", path.display()))?;
    let is_nros_toml = path.file_name().and_then(|name| name.to_str()) == Some("nros.toml");
    if is_nros_toml {
        let envelope: ComponentEnvelope =
            toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(envelope.component)
    } else {
        warn_legacy_component_manifest(path);
        let config: ComponentConfig = toml::from_str(&raw)
            .with_context(|| format!("failed to parse component manifest {}", path.display()))?;
        Ok(Some(config))
    }
}

/// Emit the `component_nros.toml` deprecation notice at most once per file path
/// for the life of the process (Phase 172 W.1 deprecation window).
fn warn_legacy_component_manifest(path: &Path) {
    static WARNED: LazyLock<Mutex<BTreeSet<PathBuf>>> =
        LazyLock::new(|| Mutex::new(BTreeSet::new()));
    if WARNED.lock().unwrap().insert(path.to_path_buf()) {
        eprintln!(
            "warning: `{}` is a deprecated standalone component manifest; fold it into the \
             package's `nros.toml` as a `[component]` table (Phase 172 W.1). The standalone \
             form still works during the deprecation window.",
            path.display()
        );
    }
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub packages: Vec<Package>,
}

#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    pub root: PathBuf,
    pub package_xml: PathBuf,
    pub nros_toml: Option<PathBuf>,
    pub launch_files: Vec<PathBuf>,
    pub manifest_files: Vec<PathBuf>,
    pub metadata_files: Vec<PathBuf>,
    /// Component-declaration candidates that tie a package to a
    /// `nros::component!` export + its source-metadata path (Phase 126.B.7).
    /// In preference order: the package's folded `nros.toml` `[component]`
    /// table (W.1), the legacy standalone `component_nros.toml`, then any
    /// `nros/components/*.toml`. An `nros.toml` without a `[component]` table
    /// is filtered out at parse time (`load_component_config`).
    pub component_config_files: Vec<PathBuf>,
}

impl Workspace {
    pub fn discover(root: &Path) -> Result<Self> {
        let mut packages = Vec::new();
        let root = root.to_path_buf();
        if root.join("package.xml").is_file() {
            packages.push(discover_package(&root)?);
        }
        let src = root.join("src");
        if src.is_dir() {
            for entry in fs::read_dir(&src)? {
                let entry = entry?;
                let path = entry.path();
                if path.join("package.xml").is_file() {
                    packages.push(discover_package(&path)?);
                }
            }
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self { root, packages })
    }

    pub fn source_metadata_files(&self) -> Vec<PathBuf> {
        unique_paths(
            self.packages
                .iter()
                .flat_map(|pkg| pkg.metadata_files.iter().cloned()),
        )
    }

    pub fn manifest_files(&self) -> Vec<PathBuf> {
        unique_paths(
            self.packages
                .iter()
                .flat_map(|pkg| pkg.manifest_files.iter().cloned()),
        )
    }

    pub fn package_nros_toml(&self, package: &str) -> Option<PathBuf> {
        self.packages
            .iter()
            .find(|pkg| pkg.name == package)
            .and_then(|pkg| pkg.nros_toml.clone())
    }

    /// Iterate every component declaration in the workspace — folded
    /// `nros.toml` `[component]` tables (W.1) and legacy standalone
    /// `component_nros.toml` / `nros/components/*.toml` files — as
    /// `(package_root, manifest_path, parsed_config)` tuples, deduped by
    /// `(package, component)`. Used by the metadata command to detect packages
    /// that
    /// declared themselves nros components but lack the
    /// `nros::component!` export (their `[metadata].source_metadata`
    /// path doesn't exist on disk — see Phase 126.B.7 acceptance
    /// criterion).
    pub fn component_declarations(&self) -> Result<Vec<ComponentDeclaration>> {
        let mut out = Vec::new();
        for pkg in &self.packages {
            // Dedup by `(package, component)` within a package, first-wins. The
            // folded `nros.toml` sorts ahead of a legacy `component_nros.toml`,
            // so when both declare the same component the folded form wins and
            // the legacy file is ignored (it still warns once on read).
            let mut seen = BTreeSet::new();
            for manifest_path in &pkg.component_config_files {
                // A package `nros.toml` is a candidate only if it actually
                // carries a `[component]` table (W.1 fold); skip it otherwise.
                let Some(config) = load_component_config(manifest_path)? else {
                    continue;
                };
                if !seen.insert((config.package.clone(), config.component.clone())) {
                    continue;
                }
                out.push(ComponentDeclaration {
                    package_root: pkg.root.clone(),
                    manifest_path: manifest_path.clone(),
                    config,
                });
            }
        }
        Ok(out)
    }
}

/// Parsed component manifest paired with its on-disk location.
#[derive(Debug, Clone)]
pub struct ComponentDeclaration {
    /// Package root the manifest belongs to. `source_metadata` paths
    /// in the manifest resolve relative to this directory.
    pub package_root: PathBuf,
    /// Absolute path to the manifest the declaration came from — a package's
    /// folded `nros.toml` (W.1) or a legacy standalone `component_nros.toml` /
    /// `nros/components/*.toml`.
    pub manifest_path: PathBuf,
    pub config: ComponentConfig,
}

impl ComponentDeclaration {
    /// Absolute path to the `[metadata].source_metadata` file the
    /// component is expected to emit. Relative paths resolve against
    /// `package_root`.
    pub fn source_metadata_path(&self) -> PathBuf {
        let raw = Path::new(&self.config.metadata.source_metadata);
        if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.package_root.join(raw)
        }
    }
}

fn discover_package(root: &Path) -> Result<Package> {
    let package_xml = root.join("package.xml");
    let parsed = PackageXml::parse(&package_xml)
        .wrap_err_with(|| format!("failed to parse {}", package_xml.display()))?;
    Ok(Package {
        name: parsed.name,
        root: root.to_path_buf(),
        package_xml,
        nros_toml: root
            .join("nros.toml")
            .is_file()
            .then(|| root.join("nros.toml")),
        launch_files: collect_files(
            root,
            &["launch"],
            &["launch.py", "launch.xml", "launch.yaml", "launch.yml"],
        )?,
        manifest_files: collect_files(
            root,
            &["manifest", "manifests"],
            &["launch.yaml", "launch.yml"],
        )?,
        metadata_files: collect_files(root, &["metadata", "nros", "target/nros"], &["json"])?,
        component_config_files: discover_component_configs(root)?,
    })
}

/// Locate component declaration candidates. Preference order (W.1 fold):
/// the package's `nros.toml` (read for a `[component]` table — the canonical
/// folded form), then the deprecated standalone `component_nros.toml` at the
/// package root, then any `nros/components/*.toml`. Whether a candidate is
/// actually a component is decided at parse time (`load_component_config`
/// returns `None` for an `nros.toml` with no `[component]`).
fn discover_component_configs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    // W.1 fold: a package `nros.toml` may carry a `[component]` table.
    let folded = root.join("nros.toml");
    if folded.is_file() {
        out.push(folded);
    }
    let primary = root.join("component_nros.toml");
    if primary.is_file() {
        out.push(primary);
    }
    // The multi-component glob is order-independent — sort it for determinism,
    // but keep it *after* the root candidates so the folded `nros.toml` and the
    // legacy `component_nros.toml` retain their preference order.
    let components_dir = root.join("nros").join("components");
    if components_dir.is_dir() {
        let mut globbed = Vec::new();
        for entry in fs::read_dir(&components_dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                globbed.push(path);
            }
        }
        globbed.sort();
        out.extend(globbed);
    }
    Ok(out)
}

fn collect_files(root: &Path, dirs: &[&str], suffixes: &[&str]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for dir in dirs {
        let path = root.join(dir);
        if path.is_dir() {
            collect_matching(&path, suffixes, &mut out)?;
        }
    }
    out.sort();
    Ok(out)
}

fn collect_matching(dir: &Path, suffixes: &[&str], out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_matching(&path, suffixes, out)?;
        } else if suffixes.iter().any(|suffix| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(suffix))
        }) {
            out.push(path);
        }
    }
    Ok(())
}

pub fn unique_paths<I>(paths: I) -> Vec<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// RAII scratch directory under the system temp dir (no `tempfile` dep).
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "nros-ws-test-{}-{}-{}",
                tag,
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn write(&self, rel: &str, body: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const PKG_XML: &str = r#"<?xml version="1.0"?>
<package format="3"><name>demo_pkg</name><version>0.0.0</version>
<description>t</description><maintainer email="a@b.c">a</maintainer><license>MIT</license>
</package>"#;

    const COMPONENT_TABLE: &str = r#"
        [component]
        version = 1
        package = "demo_pkg"
        component = "talker"
        language = "rust"
        [component.linkage]
        crate_name = "demo_pkg"
        executable = "talker"
        [component.metadata]
        source_metadata = "target/nros/metadata/talker.json"
    "#;

    // The same declaration in the legacy standalone (whole-file) shape.
    const LEGACY_WHOLE_FILE: &str = r#"
        version = 1
        package = "demo_pkg"
        component = "talker"
        language = "rust"
        [linkage]
        crate_name = "demo_pkg"
        executable = "talker"
        [metadata]
        source_metadata = "target/nros/metadata/talker.json"
    "#;

    #[test]
    fn folds_component_table_in_package_nros_toml() {
        let s = Scratch::new("fold");
        s.write("src/demo_pkg/package.xml", PKG_XML);
        // A package nros.toml carrying [workspace]-unrelated sibling tables plus
        // the folded [component] — sibling tables must be ignored.
        s.write(
            "src/demo_pkg/nros.toml",
            &format!("[[transport]]\nid = \"t\"\nkind = \"udp\"\n{COMPONENT_TABLE}"),
        );

        let ws = Workspace::discover(&s.0).unwrap();
        let decls = ws.component_declarations().unwrap();
        assert_eq!(decls.len(), 1, "folded [component] is discovered");
        assert_eq!(decls[0].config.package, "demo_pkg");
        assert_eq!(decls[0].config.component, "talker");
        assert!(decls[0].manifest_path.ends_with("nros.toml"));
    }

    #[test]
    fn legacy_component_nros_toml_still_discovered() {
        let s = Scratch::new("legacy");
        s.write("src/demo_pkg/package.xml", PKG_XML);
        s.write("src/demo_pkg/component_nros.toml", LEGACY_WHOLE_FILE);

        let decls = Workspace::discover(&s.0)
            .unwrap()
            .component_declarations()
            .unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].config.component, "talker");
        assert!(decls[0].manifest_path.ends_with("component_nros.toml"));
    }

    #[test]
    fn folded_nros_toml_wins_over_legacy_for_same_component() {
        let s = Scratch::new("both");
        s.write("src/demo_pkg/package.xml", PKG_XML);
        s.write("src/demo_pkg/nros.toml", COMPONENT_TABLE);
        s.write("src/demo_pkg/component_nros.toml", LEGACY_WHOLE_FILE);

        let decls = Workspace::discover(&s.0)
            .unwrap()
            .component_declarations()
            .unwrap();
        assert_eq!(decls.len(), 1, "duplicate (package, component) deduped");
        assert!(
            decls[0].manifest_path.ends_with("nros.toml")
                && !decls[0].manifest_path.ends_with("component_nros.toml"),
            "folded form wins: {}",
            decls[0].manifest_path.display()
        );
    }

    #[test]
    fn root_only_nros_toml_is_not_a_component() {
        let s = Scratch::new("rootonly");
        s.write("src/demo_pkg/package.xml", PKG_XML);
        // A workspace-root / direct-mode nros.toml with no [component] table.
        s.write(
            "src/demo_pkg/nros.toml",
            "[workspace]\ndefault = \"x\"\n[node]\nname = \"n\"\n",
        );

        let decls = Workspace::discover(&s.0)
            .unwrap()
            .component_declarations()
            .unwrap();
        assert!(decls.is_empty(), "no [component] table → not a component");
    }
}
