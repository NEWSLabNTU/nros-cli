//! Workspace and package discovery for host planning.

use cargo_nano_ros::package_xml::PackageXml;
use eyre::{Context, Result};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

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
    })
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
