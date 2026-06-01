//! Phase 212.B — `[workspace.metadata.nros]` + `[package.metadata.nros]` +
//! `[package.metadata.ament]` + `<bringup>/system.toml` data contracts.
//!
//! These are the *user-authored* TOML surfaces introduced by Phase 212. They
//! live in standard cargo manifest tables (`[workspace.metadata.…]` /
//! `[package.metadata.…]`) so that cargo treats them as opaque user data and
//! pure-cargo workflows (no `nros build` verb) keep working. The
//! `<bringup>/system.toml` is the per-system declarative file owned by the
//! `<system>_bringup` package.
//!
//! Vocabulary discipline (per the Phase 212 doc): every field name is a strict
//! subset of names that already appear in `nros-sdk-index.toml`,
//! `app_config.h`, or the existing planner schema. No second TOML dialect.
//!
//! Every struct here uses `#[serde(deny_unknown_fields)]` so typos surface as
//! parse errors at the user's terminal instead of being silently dropped.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::schema::RemapRule;

// ---------------------------------------------------------------------------
// Workspace-root metadata: `[workspace.metadata.nros]`
// ---------------------------------------------------------------------------

/// `[workspace.metadata.nros]` in a workspace-root `Cargo.toml`.
///
/// Thin pointer (see `docs/design/multi-node-workspace-layout.md` §5). The
/// authoritative system spec lives in `<bringup>/system.toml`; this table
/// only disambiguates which bringup the workspace defaults to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceMetadataNros {
    /// Bringup package name (`<system>_bringup`). `cargo nros plan` with no
    /// args resolves the system via this pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_system: Option<String>,
    /// Optional workspace-wide RMW override — rare, intended for
    /// `nros plan --override` workflows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rmw_override: Option<String>,
}

// ---------------------------------------------------------------------------
// Per-package metadata: `[package.metadata.nros]`
// ---------------------------------------------------------------------------

/// `[package.metadata.nros]` in a component package's `Cargo.toml`.
///
/// Two shapes (mutually exclusive):
///
/// * Single-component crate — `[package.metadata.nros.component]` describes
///   the one component the crate exposes.
/// * Multi-component crate — `[package.metadata.nros.components.<Name>]`
///   table-of-tables enumerates each.
///
/// At most one of `component` / `components` may be present; the loader
/// validates this after deserialization (a serde untagged enum would lose
/// the precise `deny_unknown_fields` error, so we keep both fields and
/// reject the conflict in [`PackageMetadataNros::validate`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMetadataNros {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentMetadata>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, ComponentMetadata>,
}

impl PackageMetadataNros {
    /// Reject manifests that set both `component` and `components` — the two
    /// shapes are mutually exclusive per the Phase 212 design doc.
    pub fn validate(&self) -> Result<(), String> {
        if self.component.is_some() && !self.components.is_empty() {
            return Err(
                "`[package.metadata.nros]` carries both `component` and \
                 `components` — use one or the other, not both"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// `[package.metadata.nros.component]` (single shape) or
/// `[package.metadata.nros.components.<Name>]` (multi shape).
///
/// Pure deployment intent — no build-system knobs (Cargo + CMake own those).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentMetadata {
    /// Default namespace the component is mounted at. Absent → `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_namespace: Option<String>,
    /// Raw ROS parameter declarations. Values stay as `toml::Value` here so
    /// the planner can do its own type-aware lowering (mirrors the existing
    /// `params::ParameterTable` resolution path).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, toml::Value>,
    /// `from` → `to` topic / service remaps, mirroring rclpy / rclcpp.
    /// Aliased to [`RemapRule`] (already `{from, to}`-shaped in
    /// `super::schema`) to avoid creating a duplicate type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remaps: Vec<RemapRule>,
}

impl Default for ComponentMetadata {
    fn default() -> Self {
        Self {
            default_namespace: None,
            parameters: BTreeMap::new(),
            remaps: Vec::new(),
        }
    }
}

/// Convenience alias: spec calls these `RemapEntry`. The existing
/// `RemapRule` already has the right shape, so we expose both names.
pub type RemapEntry = RemapRule;

// ---------------------------------------------------------------------------
// Per-package metadata: `[package.metadata.ament]`
// ---------------------------------------------------------------------------

/// `[package.metadata.ament]` — the source of truth for `nros emit
/// package-xml` (Phase 212.G). Mirrors ament/colcon's `package.xml`
/// vocabulary 1-to-1.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMetadataAment {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_depend: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exec_depend: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_depend: Vec<String>,
    /// e.g. `"ament_cargo"`, `"ament_cmake"`, `"ament_nros"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Per-bringup file: `<bringup>/system.toml`
// ---------------------------------------------------------------------------

/// `<bringup>/system.toml` — the authoritative system spec.
///
/// Sections (see `docs/design/workspace-layout-by-case.md` Case 3/4 and
/// `multi-node-workspace-layout.md` §4):
///
/// * `[system]` — name, RMW, domain, optional locator.
/// * `[[component]]` — one entry per node/component.
/// * `[deploy.<target>]` — per-target deploy block (`kind = "self" | "qemu"
///   | "flash" | …`).
/// * `[[domain]]` — optional per-system domain routing.
/// * `[[bridge]]` — optional cross-RMW bridges.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemToml {
    pub system: SystemHeader,
    #[serde(default, rename = "component", skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<SystemComponentEntry>,
    /// `[deploy.<target>]` — keyed by target name (e.g. `native`,
    /// `qemu-mps2-an385`, `flash-stm32f4-disco`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub deploy: BTreeMap<String, DeployTarget>,
    #[serde(default, rename = "domain", skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<SystemDomainEntry>,
    #[serde(default, rename = "bridge", skip_serializing_if = "Vec::is_empty")]
    pub bridges: Vec<SystemBridgeEntry>,
}

/// `[system]` table inside `<bringup>/system.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemHeader {
    pub name: String,
    pub rmw: String,
    pub domain_id: u32,
    /// Optional default locator. Per-deploy blocks can override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
}

/// `[[component]]` row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemComponentEntry {
    pub pkg: String,
    pub class: String,
    pub name: String,
}

/// `[deploy.<target>]` block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployTarget {
    /// `"self"`, `"qemu"`, `"flash"`, … — interpreted by the runner stage.
    pub kind: String,
    /// Target triple / board id / runner key. The exact semantics depend on
    /// `kind`.
    pub target: String,
    /// Optional path (relative to the bringup pkg root) to a
    /// `launch/*.launch.xml` used for this deploy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<String>,
    /// Optional board identifier (e.g. `mps2_an385`, `qemu_riscv64`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
}

/// `[[domain]]` row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemDomainEntry {
    pub name: String,
    pub rmw: String,
    pub id: u32,
}

/// `[[bridge]]` row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemBridgeEntry {
    pub name: String,
    pub from: String,
    pub to: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a `[workspace.metadata.nros]` golden through parse +
    /// serialize + reparse and compare structs.
    #[test]
    fn workspace_metadata_round_trip() {
        let raw = r#"
default_system = "demo_bringup"
rmw_override = "cyclonedds"
"#;
        let v1: WorkspaceMetadataNros = toml::from_str(raw).expect("parse golden");
        assert_eq!(v1.default_system.as_deref(), Some("demo_bringup"));
        assert_eq!(v1.rmw_override.as_deref(), Some("cyclonedds"));

        let reserialized = toml::to_string(&v1).expect("serialize");
        let v2: WorkspaceMetadataNros = toml::from_str(&reserialized).expect("reparse");
        assert_eq!(v1, v2);
    }

    /// Minimal workspace-metadata table (only `default_system`) parses; the
    /// optional `rmw_override` defaults to `None`.
    #[test]
    fn workspace_metadata_minimal_parses() {
        let raw = r#"default_system = "demo_bringup""#;
        let v: WorkspaceMetadataNros = toml::from_str(raw).expect("parse");
        assert_eq!(v.default_system.as_deref(), Some("demo_bringup"));
        assert!(v.rmw_override.is_none());
    }

    /// Empty workspace-metadata is also valid (workspace may declare the
    /// table without populating it yet).
    #[test]
    fn workspace_metadata_empty_parses() {
        let v: WorkspaceMetadataNros = toml::from_str("").expect("parse empty");
        assert_eq!(v, WorkspaceMetadataNros::default());
    }

    /// `[package.metadata.nros.component]` single-shape round-trip.
    #[test]
    fn package_metadata_single_component_round_trip() {
        let raw = r#"
[component]
default_namespace = "/demo"

[component.parameters]
rate_hz = 10
greeting = "hello"

[[component.remaps]]
from = "chatter"
to = "topic/chatter"
"#;
        let v1: PackageMetadataNros = toml::from_str(raw).expect("parse");
        v1.validate().expect("single-shape is valid");
        let component = v1.component.as_ref().expect("component present");
        assert_eq!(component.default_namespace.as_deref(), Some("/demo"));
        assert_eq!(component.parameters.len(), 2);
        assert_eq!(component.remaps.len(), 1);
        assert_eq!(component.remaps[0].from, "chatter");
        assert_eq!(component.remaps[0].to, "topic/chatter");
        assert!(v1.components.is_empty());

        let reserialized = toml::to_string(&v1).expect("serialize");
        let v2: PackageMetadataNros = toml::from_str(&reserialized).expect("reparse");
        assert_eq!(v1, v2);
    }

    /// `[package.metadata.nros.components.<Name>]` multi-shape round-trip.
    #[test]
    fn package_metadata_multi_component_round_trip() {
        let raw = r#"
[components.Talker]
default_namespace = "/demo"

[components.Talker.parameters]
rate_hz = 10

[components.Listener]
default_namespace = "/demo"
"#;
        let v1: PackageMetadataNros = toml::from_str(raw).expect("parse");
        v1.validate().expect("multi-shape is valid");
        assert!(v1.component.is_none());
        assert_eq!(v1.components.len(), 2);
        // BTreeMap ⇒ keys are sorted.
        let names: Vec<&str> = v1.components.keys().map(String::as_str).collect();
        assert_eq!(names, ["Listener", "Talker"]);

        let reserialized = toml::to_string(&v1).expect("serialize");
        let v2: PackageMetadataNros = toml::from_str(&reserialized).expect("reparse");
        assert_eq!(v1, v2);
    }

    /// Declaring both `component` and `components` is a hard error (loader
    /// must call `validate`).
    #[test]
    fn package_metadata_rejects_both_shapes() {
        let raw = r#"
[component]
default_namespace = "/a"

[components.Other]
default_namespace = "/b"
"#;
        let v: PackageMetadataNros = toml::from_str(raw).expect("parse");
        let err = v.validate().expect_err("conflicting shapes must error");
        assert!(err.contains("component"), "diagnostic mentions field: {err}");
    }

    /// `deny_unknown_fields` rejects typos on the component table.
    #[test]
    fn rejects_unknown_field_in_strict_mode() {
        let raw = r#"
[component]
default_namespace = "/demo"
unknown_typo = true
"#;
        let err = toml::from_str::<PackageMetadataNros>(raw)
            .expect_err("unknown field must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown_typo") || msg.contains("unknown field"),
            "diagnostic should name the typo: {msg}"
        );
    }

    /// Same strictness for the workspace table.
    #[test]
    fn rejects_unknown_field_on_workspace_metadata() {
        let raw = r#"
default_system = "demo_bringup"
not_a_field = 42
"#;
        let err = toml::from_str::<WorkspaceMetadataNros>(raw)
            .expect_err("unknown field must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("not_a_field") || msg.contains("unknown field"),
            "diagnostic: {msg}"
        );
    }

    /// `[package.metadata.ament]` round-trip.
    #[test]
    fn ament_metadata_round_trip() {
        let raw = r#"
build_depend = ["rosidl_default_generators"]
exec_depend = ["rosidl_default_runtime", "std_msgs"]
test_depend = ["ament_lint_auto"]
build_type = "ament_cargo"
"#;
        let v1: PackageMetadataAment = toml::from_str(raw).expect("parse");
        assert_eq!(v1.build_depend, vec!["rosidl_default_generators"]);
        assert_eq!(v1.exec_depend, vec!["rosidl_default_runtime", "std_msgs"]);
        assert_eq!(v1.test_depend, vec!["ament_lint_auto"]);
        assert_eq!(v1.build_type.as_deref(), Some("ament_cargo"));

        let reserialized = toml::to_string(&v1).expect("serialize");
        let v2: PackageMetadataAment = toml::from_str(&reserialized).expect("reparse");
        assert_eq!(v1, v2);
    }

    /// Minimal `[package.metadata.ament]` (only `exec_depend`) parses.
    #[test]
    fn ament_metadata_minimal_parses() {
        let raw = r#"exec_depend = ["std_msgs"]"#;
        let v: PackageMetadataAment = toml::from_str(raw).expect("parse");
        assert_eq!(v.exec_depend, vec!["std_msgs"]);
        assert!(v.build_depend.is_empty());
        assert!(v.test_depend.is_empty());
        assert!(v.build_type.is_none());
    }

    /// Full `<bringup>/system.toml` golden round-trip.
    #[test]
    fn system_toml_round_trip() {
        let raw = r#"
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
launch = "launch/system.launch.xml"

[deploy.qemu-mps2-an385]
kind = "qemu"
target = "thumbv7m-none-eabi"
board = "mps2_an385"

[[domain]]
name = "default"
rmw = "zenoh"
id = 0

[[bridge]]
name = "cyclone_to_zenoh"
from = "cyclone:default"
to = "zenoh:default"
"#;
        let v1: SystemToml = toml::from_str(raw).expect("parse system.toml");
        assert_eq!(v1.system.name, "demo");
        assert_eq!(v1.system.rmw, "zenoh");
        assert_eq!(v1.system.domain_id, 0);
        assert_eq!(v1.system.locator.as_deref(), Some("tcp/127.0.0.1:7447"));
        assert_eq!(v1.components.len(), 2);
        assert_eq!(v1.components[0].name, "talker");
        assert_eq!(v1.components[1].name, "listener");
        assert_eq!(v1.deploy.len(), 2);
        let native = v1.deploy.get("native").expect("native deploy present");
        assert_eq!(native.kind, "self");
        assert_eq!(native.launch.as_deref(), Some("launch/system.launch.xml"));
        let qemu = v1.deploy.get("qemu-mps2-an385").expect("qemu deploy present");
        assert_eq!(qemu.board.as_deref(), Some("mps2_an385"));
        assert_eq!(v1.domains.len(), 1);
        assert_eq!(v1.bridges.len(), 1);

        let reserialized = toml::to_string(&v1).expect("serialize");
        let v2: SystemToml = toml::from_str(&reserialized).expect("reparse");
        assert_eq!(v1, v2);
    }

    /// Minimal `<bringup>/system.toml` — only `[system]` + one
    /// `[[component]]`, optional sections absent.
    #[test]
    fn system_toml_minimal_parses() {
        let raw = r#"
[system]
name = "demo"
rmw = "zenoh"
domain_id = 0

[[component]]
pkg = "talker_pkg"
class = "talker_pkg::TalkerNode"
name = "talker"
"#;
        let v: SystemToml = toml::from_str(raw).expect("parse minimal");
        assert_eq!(v.system.name, "demo");
        assert!(v.system.locator.is_none());
        assert_eq!(v.components.len(), 1);
        assert!(v.deploy.is_empty());
        assert!(v.domains.is_empty());
        assert!(v.bridges.is_empty());
    }

    /// `deny_unknown_fields` on `[system]` catches typos at the bringup
    /// surface.
    #[test]
    fn system_toml_rejects_unknown_field() {
        let raw = r#"
[system]
name = "demo"
rmw = "zenoh"
domain_id = 0
mystery_knob = "no"
"#;
        let err = toml::from_str::<SystemToml>(raw)
            .expect_err("unknown field on [system] must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("mystery_knob") || msg.contains("unknown field"),
            "diagnostic: {msg}"
        );
    }
}
