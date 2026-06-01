//! Phase 212.B.2 — `NrosConfig::from_cargo_metadata` loader.
//!
//! Reads the user-authored Phase 212 surfaces out of a cargo workspace:
//!
//! * `[workspace.metadata.nros]` in the workspace-root `Cargo.toml`
//! * `[package.metadata.nros]` in every workspace-member `Cargo.toml`
//!   (single-shape `component` or multi-shape `components.<Name>`)
//! * `[package.metadata.ament]` in every workspace-member `Cargo.toml`
//! * `<bringup-pkg>/system.toml` for every bringup package the workspace
//!   exposes (a bringup package is a workspace member whose
//!   `package.metadata.nros` is absent and which carries a sibling
//!   `system.toml` next to its `Cargo.toml`).
//!
//! No silent fallback to the old `nros.toml` surface (Phase 172). A
//! workspace whose root carries a `nros.toml` next to its `Cargo.toml`
//! is rejected with a migration pointer (see Phase 212.I).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use cargo_metadata::MetadataCommand;
use serde::Deserialize;
use thiserror::Error;

use super::cargo_metadata_schema::{
    PackageMetadataAment, PackageMetadataNros, SystemToml, WorkspaceMetadataNros,
};

/// Errors surfaced by the Phase 212.B loader. Distinct from the catch-all
/// `eyre::Result` so callers can match on `NrosTomlNotSupported` and route to
/// the migration tool (Phase 212.I).
#[derive(Debug, Error)]
pub enum NrosConfigError {
    /// A pre-212 `nros.toml` sits at the workspace root. Clean break — point
    /// the user at the migration tool.
    #[error(
        "nros.toml at workspace root is no longer supported; run \
         `nros migrate workspace .` to convert to the new shape \
         (Phase 212.B → see docs/roadmap/phase-212-ux-cargo-native-and-file-consolidation.md)"
    )]
    NrosTomlNotSupported { path: PathBuf },

    /// `cargo metadata` failed (bad manifest, missing cargo, etc.). The
    /// underlying error is preserved.
    #[error("cargo metadata at {path:?}: {source}")]
    CargoMetadata {
        path: PathBuf,
        #[source]
        source: cargo_metadata::Error,
    },

    /// A `[workspace.metadata.nros]` table was present but failed to
    /// deserialize against the strict schema (typo / unknown field).
    #[error("invalid [workspace.metadata.nros] in {manifest:?}: {message}")]
    InvalidWorkspaceMetadata { manifest: PathBuf, message: String },

    /// A per-package `[package.metadata.nros]` failed to deserialize or
    /// failed the mutual-exclusion check between `component` and `components`.
    #[error("invalid [package.metadata.nros] in package `{package}` ({manifest:?}): {message}")]
    InvalidPackageMetadata {
        package: String,
        manifest: PathBuf,
        message: String,
    },

    /// `[package.metadata.ament]` failed to deserialize.
    #[error("invalid [package.metadata.ament] in package `{package}` ({manifest:?}): {message}")]
    InvalidAmentMetadata {
        package: String,
        manifest: PathBuf,
        message: String,
    },

    /// A bringup package's `system.toml` could not be read.
    #[error("read {path:?} for bringup package `{package}`: {source}")]
    BringupSystemTomlIo {
        package: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A bringup package's `system.toml` failed to parse.
    #[error("parse {path:?} for bringup package `{package}`: {source}")]
    BringupSystemTomlParse {
        package: String,
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Phase 212.B `NrosConfig` — every nros-relevant fact derived from a
/// cargo workspace. Replaces the `nros.toml` reader from Phase 172.
///
/// The data is purely descriptive: callers (`nros plan`, `nros check`,
/// `nros codegen system`, …) consume this to do their work. The loader
/// performs strict schema validation but no cross-component semantic
/// validation — that is the planner's job.
#[derive(Clone, Debug, Default)]
pub struct NrosConfig {
    /// Workspace root directory (where `Cargo.toml` lives).
    pub workspace_root: PathBuf,
    /// `[workspace.metadata.nros]` — absent in some workspaces (treated as
    /// `WorkspaceMetadataNros::default()`).
    pub workspace_metadata: WorkspaceMetadataNros,
    /// Component packages: workspace members carrying
    /// `[package.metadata.nros]`. Keyed by cargo package name.
    pub component_packages: BTreeMap<String, ComponentPackageEntry>,
    /// Bringup packages: workspace members WITHOUT `[package.metadata.nros]`
    /// that carry a sibling `system.toml`. Keyed by cargo package name.
    pub bringup_packages: BTreeMap<String, BringupPackageEntry>,
}

/// A workspace member that exposes one or more nros components via its
/// `[package.metadata.nros]` table.
#[derive(Clone, Debug)]
pub struct ComponentPackageEntry {
    pub name: String,
    pub manifest_path: PathBuf,
    pub nros: PackageMetadataNros,
    pub ament: PackageMetadataAment,
}

/// A workspace member declared as the bringup pkg for a system. Carries a
/// loaded `system.toml`.
#[derive(Clone, Debug)]
pub struct BringupPackageEntry {
    pub name: String,
    pub manifest_path: PathBuf,
    /// `<bringup>/system.toml`.
    pub system_toml_path: PathBuf,
    pub system: SystemToml,
    /// Bringup pkgs may still declare `[package.metadata.ament]` for
    /// `package.xml` regeneration (Phase 212.G).
    pub ament: PackageMetadataAment,
}

impl NrosConfig {
    /// Load `NrosConfig` from a cargo workspace rooted at `workspace_root`.
    ///
    /// Steps:
    ///
    /// 1. Reject a `nros.toml` sitting next to the workspace `Cargo.toml`
    ///    with a migration pointer (Phase 212.I).
    /// 2. Shell `cargo metadata --no-deps --format-version 1` via the
    ///    `cargo_metadata` crate.
    /// 3. Parse `metadata.workspace_metadata["nros"]` into
    ///    [`WorkspaceMetadataNros`].
    /// 4. For each workspace member, parse `package.metadata["nros"]` into
    ///    [`PackageMetadataNros`] and `package.metadata["ament"]` into
    ///    [`PackageMetadataAment`].
    /// 5. A member with no `[package.metadata.nros]` AND a sibling
    ///    `system.toml` becomes a bringup pkg (loaded into [`SystemToml`]).
    pub fn from_cargo_metadata(workspace_root: &Path) -> Result<Self, NrosConfigError> {
        // 1 — clean-break rejection of the old root nros.toml.
        let root_nros_toml = workspace_root.join("nros.toml");
        if root_nros_toml.exists() {
            return Err(NrosConfigError::NrosTomlNotSupported {
                path: root_nros_toml,
            });
        }

        let manifest_path = workspace_root.join("Cargo.toml");

        // 2 — run cargo metadata.
        let metadata = MetadataCommand::new()
            .manifest_path(&manifest_path)
            .no_deps()
            .exec()
            .map_err(|source| NrosConfigError::CargoMetadata {
                path: manifest_path.clone(),
                source,
            })?;

        // 3 — workspace-level metadata.
        let workspace_metadata = parse_workspace_metadata(&metadata.workspace_metadata)
            .map_err(|message| NrosConfigError::InvalidWorkspaceMetadata {
                manifest: manifest_path.clone(),
                message,
            })?;

        // 4 — per-member metadata + 5 bringup discovery.
        let mut component_packages: BTreeMap<String, ComponentPackageEntry> = BTreeMap::new();
        let mut bringup_packages: BTreeMap<String, BringupPackageEntry> = BTreeMap::new();

        let member_ids: std::collections::HashSet<&cargo_metadata::PackageId> =
            metadata.workspace_members.iter().collect();

        for package in &metadata.packages {
            if !member_ids.contains(&package.id) {
                continue;
            }

            let pkg_manifest = PathBuf::from(package.manifest_path.as_str());

            let ament = parse_ament_metadata(&package.metadata).map_err(|message| {
                NrosConfigError::InvalidAmentMetadata {
                    package: package.name.clone(),
                    manifest: pkg_manifest.clone(),
                    message,
                }
            })?;

            let nros_opt = parse_package_metadata_nros(&package.metadata).map_err(|message| {
                NrosConfigError::InvalidPackageMetadata {
                    package: package.name.clone(),
                    manifest: pkg_manifest.clone(),
                    message,
                }
            })?;

            match nros_opt {
                Some(nros) => {
                    // Validate single-vs-multi shape exclusion.
                    nros.validate().map_err(|message| {
                        NrosConfigError::InvalidPackageMetadata {
                            package: package.name.clone(),
                            manifest: pkg_manifest.clone(),
                            message,
                        }
                    })?;
                    component_packages.insert(
                        package.name.clone(),
                        ComponentPackageEntry {
                            name: package.name.clone(),
                            manifest_path: pkg_manifest,
                            nros,
                            ament,
                        },
                    );
                }
                None => {
                    // Bringup-pkg candidate: look for a sibling `system.toml`.
                    let pkg_dir = pkg_manifest.parent().unwrap_or_else(|| Path::new(""));
                    let system_toml_path = pkg_dir.join("system.toml");
                    if system_toml_path.exists() {
                        let raw = std::fs::read_to_string(&system_toml_path).map_err(|source| {
                            NrosConfigError::BringupSystemTomlIo {
                                package: package.name.clone(),
                                path: system_toml_path.clone(),
                                source,
                            }
                        })?;
                        let system: SystemToml = toml::from_str(&raw).map_err(|source| {
                            NrosConfigError::BringupSystemTomlParse {
                                package: package.name.clone(),
                                path: system_toml_path.clone(),
                                source,
                            }
                        })?;
                        bringup_packages.insert(
                            package.name.clone(),
                            BringupPackageEntry {
                                name: package.name.clone(),
                                manifest_path: pkg_manifest,
                                system_toml_path,
                                system,
                                ament,
                            },
                        );
                    }
                    // Else: a plain workspace member with no nros surface —
                    // ignored (it may be a util/lib crate the bringup pulls in).
                }
            }
        }

        Ok(NrosConfig {
            workspace_root: workspace_root.to_path_buf(),
            workspace_metadata,
            component_packages,
            bringup_packages,
        })
    }
}

// ---------------------------------------------------------------------------
// `cargo metadata` JSON helpers
// ---------------------------------------------------------------------------

/// `metadata.workspace_metadata` is a free-form `serde_json::Value`. Pull
/// `nros` out and re-parse via the strict schema. Returns the default when
/// the table is absent.
fn parse_workspace_metadata(value: &serde_json::Value) -> Result<WorkspaceMetadataNros, String> {
    let Some(nros) = value.get("nros") else {
        return Ok(WorkspaceMetadataNros::default());
    };
    WorkspaceMetadataNros::deserialize(nros.clone()).map_err(|e| e.to_string())
}

/// `package.metadata` likewise. Returns `Ok(None)` when the `nros` key is
/// absent.
fn parse_package_metadata_nros(
    value: &serde_json::Value,
) -> Result<Option<PackageMetadataNros>, String> {
    let Some(nros) = value.get("nros") else {
        return Ok(None);
    };
    PackageMetadataNros::deserialize(nros.clone())
        .map(Some)
        .map_err(|e| e.to_string())
}

fn parse_ament_metadata(value: &serde_json::Value) -> Result<PackageMetadataAment, String> {
    let Some(ament) = value.get("ament") else {
        return Ok(PackageMetadataAment::default());
    };
    PackageMetadataAment::deserialize(ament.clone()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a throwaway directory under `target/` per test (cargo cleans
    /// `target/` between runs and the path is unique per test name).
    fn scratch_dir(test: &str) -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join("nros-cli-core-tests")
            });
        let dir = base.join(format!("nros_config_{test}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Write the minimal "1 root + 2 component crates + 1 bringup pkg" cargo
    /// workspace into `dir`.
    fn write_minimal_workspace(dir: &Path) {
        // Workspace root manifest.
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

        // talker_pkg — single-component shape.
        fs::create_dir_all(dir.join("talker_pkg/src")).unwrap();
        fs::write(
            dir.join("talker_pkg/Cargo.toml"),
            r#"
[package]
name = "talker_pkg"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[package.metadata.nros.component]
default_namespace = "/demo"

[package.metadata.nros.component.parameters]
rate_hz = 10

[[package.metadata.nros.component.remaps]]
from = "chatter"
to = "topic/chatter"

[package.metadata.ament]
build_depend = ["rosidl_default_generators"]
exec_depend = ["std_msgs"]
"#,
        )
        .unwrap();
        fs::write(dir.join("talker_pkg/src/lib.rs"), "").unwrap();

        // listener_pkg — multi-component shape.
        fs::create_dir_all(dir.join("listener_pkg/src")).unwrap();
        fs::write(
            dir.join("listener_pkg/Cargo.toml"),
            r#"
[package]
name = "listener_pkg"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[package.metadata.nros.components.Listener]
default_namespace = "/demo"

[package.metadata.nros.components.Echo]
default_namespace = "/demo"
"#,
        )
        .unwrap();
        fs::write(dir.join("listener_pkg/src/lib.rs"), "").unwrap();

        // demo_bringup — no [package.metadata.nros], system.toml sibling.
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

[package.metadata.ament]
exec_depend = ["talker_pkg", "listener_pkg"]
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
domain_id = 0
locator = "tcp/127.0.0.1:7447"

[[component]]
pkg = "talker_pkg"
class = "talker_pkg::TalkerNode"
name = "talker"

[[component]]
pkg = "listener_pkg"
class = "listener_pkg::ListenerNode"
name = "listener"

[deploy.native]
kind = "self"
target = "x86_64-unknown-linux-gnu"
"#,
        )
        .unwrap();
    }

    /// 212.B.2 — golden fixture w/ root Cargo.toml + 2 component crates +
    /// bringup pkg loads end-to-end.
    #[test]
    fn load_workspace_from_minimal_cargo_metadata() {
        let dir = scratch_dir("load_workspace_from_minimal_cargo_metadata");
        write_minimal_workspace(&dir);

        let cfg = NrosConfig::from_cargo_metadata(&dir).expect("loads minimal workspace");

        assert_eq!(cfg.workspace_root, dir);
        assert_eq!(
            cfg.workspace_metadata.default_system.as_deref(),
            Some("demo_bringup")
        );
        assert!(cfg.workspace_metadata.rmw_override.is_none());

        // Two component packages, one bringup.
        assert_eq!(cfg.component_packages.len(), 2, "talker + listener");
        assert!(cfg.component_packages.contains_key("talker_pkg"));
        assert!(cfg.component_packages.contains_key("listener_pkg"));
        assert_eq!(cfg.bringup_packages.len(), 1);
        assert!(cfg.bringup_packages.contains_key("demo_bringup"));
    }

    /// 212.B.2 — `nros.toml` at workspace root is rejected with a migration
    /// pointer (no silent fallback).
    #[test]
    fn nros_toml_at_root_rejected_with_migration_pointer() {
        let dir = scratch_dir("nros_toml_at_root_rejected_with_migration_pointer");
        write_minimal_workspace(&dir);

        // Drop a pre-212 `nros.toml` next to the workspace root.
        fs::write(dir.join("nros.toml"), "[workspace]\n").unwrap();

        let err = NrosConfig::from_cargo_metadata(&dir)
            .expect_err("must reject root nros.toml");
        match &err {
            NrosConfigError::NrosTomlNotSupported { path } => {
                assert_eq!(path, &dir.join("nros.toml"));
            }
            other => panic!("expected NrosTomlNotSupported, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains("nros migrate workspace"),
            "diagnostic must mention the migration tool: {msg}"
        );
        assert!(
            msg.contains("Phase 212.B") || msg.contains("phase-212"),
            "diagnostic must point at the phase doc: {msg}"
        );
    }

    /// 212.B.2 — single-component-shape `[package.metadata.nros.component]`
    /// parses and round-trips through the loader.
    #[test]
    fn single_component_via_package_metadata_nros_component() {
        let dir = scratch_dir("single_component_via_package_metadata_nros_component");
        write_minimal_workspace(&dir);

        let cfg = NrosConfig::from_cargo_metadata(&dir).expect("loads");

        let talker = cfg
            .component_packages
            .get("talker_pkg")
            .expect("talker present");
        let component = talker
            .nros
            .component
            .as_ref()
            .expect("single-shape component table present");
        assert_eq!(component.default_namespace.as_deref(), Some("/demo"));
        assert_eq!(component.parameters.get("rate_hz").map(|v| v.as_integer()), Some(Some(10)));
        assert_eq!(component.remaps.len(), 1);
        assert_eq!(component.remaps[0].from, "chatter");
        assert!(talker.nros.components.is_empty());

        // The ament side rides through too.
        assert_eq!(talker.ament.build_depend, vec!["rosidl_default_generators"]);
        assert_eq!(talker.ament.exec_depend, vec!["std_msgs"]);
    }

    /// 212.B.2 — multi-component-shape `[package.metadata.nros.components.<N>]`
    /// table-of-tables parses.
    #[test]
    fn multi_component_via_package_metadata_nros_components() {
        let dir = scratch_dir("multi_component_via_package_metadata_nros_components");
        write_minimal_workspace(&dir);

        let cfg = NrosConfig::from_cargo_metadata(&dir).expect("loads");

        let listener = cfg
            .component_packages
            .get("listener_pkg")
            .expect("listener present");
        assert!(listener.nros.component.is_none());
        // BTreeMap-sorted keys.
        let names: Vec<&str> = listener.nros.components.keys().map(String::as_str).collect();
        assert_eq!(names, ["Echo", "Listener"]);
    }

    /// 212.B.2 — bringup pkg's `system.toml` is loaded into the entry.
    #[test]
    fn bringup_pkg_loaded_from_system_toml() {
        let dir = scratch_dir("bringup_pkg_loaded_from_system_toml");
        write_minimal_workspace(&dir);

        let cfg = NrosConfig::from_cargo_metadata(&dir).expect("loads");

        let bringup = cfg
            .bringup_packages
            .get("demo_bringup")
            .expect("demo_bringup present");
        assert_eq!(bringup.system.system.name, "demo");
        assert_eq!(bringup.system.system.rmw, "zenoh");
        assert_eq!(bringup.system.system.domain_id, 0);
        assert_eq!(
            bringup.system.system.locator.as_deref(),
            Some("tcp/127.0.0.1:7447")
        );
        assert_eq!(bringup.system.components.len(), 2);
        assert_eq!(bringup.system.components[0].name, "talker");
        assert_eq!(bringup.system.components[1].name, "listener");
        let native = bringup
            .system
            .deploy
            .get("native")
            .expect("native deploy present");
        assert_eq!(native.kind, "self");

        // The bringup pkg's ament block is preserved.
        assert_eq!(bringup.ament.exec_depend, vec!["talker_pkg", "listener_pkg"]);
        // And the system.toml path is recorded (callers regenerating
        // package.xml from system.toml need it).
        assert_eq!(
            bringup.system_toml_path,
            dir.join("demo_bringup/system.toml")
        );
    }
}
