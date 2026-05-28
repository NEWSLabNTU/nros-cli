use serde::{Deserialize, Serialize};

use super::{
    schema::{DeadlinePolicy, ParameterTable, RemapRule, SchedClass},
    source_metadata::ComponentLanguage,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentConfig {
    pub version: u32,
    pub package: String,
    pub component: String,
    pub language: ComponentLanguage,
    pub linkage: ComponentLinkage,
    pub metadata: ComponentMetadataConfig,
    // W.3 (Phase 172): an absent `[overrides]` is legal — a minimal component
    // manifest declares only linkage + metadata. Defaults to empty overrides.
    #[serde(default)]
    pub overrides: ComponentOverrides,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentLinkage {
    pub crate_name: Option<String>,
    pub executable: Option<String>,
    pub exported_symbol: Option<String>,
    pub static_library: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentMetadataConfig {
    pub source_metadata: String,
    pub generated_by: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_namespace: Option<String>,
    // W.3 (Phase 172): `parameters`/`remaps` default to empty so a minimal
    // `[overrides]` (or none at all) is legal — previously a manifest without
    // them failed with *"missing field `parameters`"*.
    #[serde(default, skip_serializing_if = "ParameterTable::is_empty")]
    pub parameters: ParameterTable,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remaps: Vec<RemapRule>,
}

// Phase 172 flip: the per-package `SystemConfig` tree (`TargetConfig`'s
// triple/board, `ManifestSource`, `SystemComponent`, `SystemOverlay`,
// `InstanceSelector`, `SchedulingSelector`, `SchedulingConfig`,
// `EndpointMapping`, `BuildConfig`) is retired. Deployment config now lives in
// the root `nros.toml` (`[deploy.<name>]` carries target/board; the planner
// derives the plan's `[build]` from nros.toml overlays). What remains here is
// the component manifest (`ComponentConfig`) + the scheduling tier
// (`SchedContextConfig`, consumed by 172.G + `root_config`).

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedContextConfig {
    pub id: String,
    pub executor: String,
    pub class: SchedClass,
    pub priority: Option<u8>,
    pub period_ms: Option<u64>,
    pub budget_ms: Option<u64>,
    pub deadline_ms: Option<u64>,
    pub deadline_policy: DeadlinePolicy,
    pub stack_size: Option<u32>,
    pub core: Option<u32>,
    pub task: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W.3 (Phase 172): a minimal component manifest declares only linkage +
    /// metadata — no `[overrides]` table at all — and parses with empty
    /// overrides instead of failing *"missing field `parameters`"*.
    #[test]
    fn minimal_component_manifest_without_overrides_parses() {
        let raw = r#"
            version = 1
            package = "demo_nodes_rs"
            component = "talker"
            language = "rust"

            [linkage]
            crate_name = "demo_nodes_rs"
            executable = "talker"

            [metadata]
            source_metadata = "target/nros/metadata/talker.json"
        "#;
        let cfg: ComponentConfig = toml::from_str(raw).expect("minimal manifest parses");
        assert_eq!(cfg.overrides, ComponentOverrides::default());
        assert!(cfg.overrides.parameters.is_empty());
        assert!(cfg.overrides.remaps.is_empty());
        assert!(cfg.overrides.default_namespace.is_none());
    }

    /// An `[overrides]` table that sets only `default_namespace` (no
    /// `parameters`/`remaps`) is also legal now.
    #[test]
    fn partial_overrides_defaults_missing_fields() {
        let raw = r#"
            version = 1
            package = "p"
            component = "c"
            language = "rust"

            [linkage]
            crate_name = "p"

            [metadata]
            source_metadata = "m.json"

            [overrides]
            default_namespace = "/demo"
        "#;
        let cfg: ComponentConfig = toml::from_str(raw).expect("partial overrides parse");
        assert_eq!(cfg.overrides.default_namespace.as_deref(), Some("/demo"));
        assert!(cfg.overrides.parameters.is_empty());
        assert!(cfg.overrides.remaps.is_empty());
    }
}
