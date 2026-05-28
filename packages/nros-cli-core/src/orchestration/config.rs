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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentOverrides {
    pub default_namespace: Option<String>,
    pub parameters: ParameterTable,
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
