//! Workspace and package discovery for host planning.

use cargo_nano_ros::package_xml::PackageXml;
use eyre::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::config::ComponentConfig;

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
    /// `component_nros.toml` manifests in the package root (Phase
    /// 126.B.7 — component declaration files that tie a package to a
    /// `nros::component!` export + its source metadata path).
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

    /// Iterate every `component_nros.toml` declaration in the
    /// workspace as `(package_root, manifest_path, parsed_config)`
    /// tuples. Used by the metadata command to detect packages that
    /// declared themselves nros components but lack the
    /// `nros::component!` export (their `[metadata].source_metadata`
    /// path doesn't exist on disk — see Phase 126.B.7 acceptance
    /// criterion).
    pub fn component_declarations(&self) -> Result<Vec<ComponentDeclaration>> {
        let mut out = Vec::new();
        for pkg in &self.packages {
            for manifest_path in &pkg.component_config_files {
                let raw = fs::read_to_string(manifest_path).with_context(|| {
                    format!(
                        "failed to read component manifest {}",
                        manifest_path.display()
                    )
                })?;
                let config: ComponentConfig = toml::from_str(&raw).with_context(|| {
                    format!(
                        "failed to parse component manifest {}",
                        manifest_path.display()
                    )
                })?;
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
    /// Absolute path to the `component_nros.toml` file.
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

/// Locate component declaration files (`component_nros.toml` at the
/// package root, plus any `nros/components/*.toml`). Keeps the layout
/// flexible — single-component packages drop one TOML at the root;
/// multi-component packages place per-component manifests under
/// `nros/components/`.
fn discover_component_configs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let primary = root.join("component_nros.toml");
    if primary.is_file() {
        out.push(primary);
    }
    let components_dir = root.join("nros").join("components");
    if components_dir.is_dir() {
        for entry in fs::read_dir(&components_dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                out.push(path);
            }
        }
    }
    out.sort();
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
