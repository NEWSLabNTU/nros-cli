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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemConfig {
    pub version: u32,
    pub system: String,
    pub target: TargetConfig,
    pub manifests: Vec<ManifestSource>,
    pub components: Vec<SystemComponent>,
    pub overlays: Vec<SystemOverlay>,
    pub scheduling: SchedulingConfig,
    pub endpoint_mappings: Vec<EndpointMapping>,
    pub build: BuildConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    pub triple: String,
    pub board: String,
    pub rmw: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSource {
    pub package: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemComponent {
    pub package: String,
    pub component: String,
    pub config: String,
    /// Phase 172.K.4 — transport `id` this component's nodes bind to (which
    /// `[[transport]]` session their entities ride). `None` ⇒ the default /
    /// single transport. Full per-node `create_node_on`-by-id binding is K.5;
    /// this carries the binding through the system config + plan.
    #[serde(default)]
    pub transport: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemOverlay {
    pub selector: InstanceSelector,
    pub namespace: Option<String>,
    pub parameters: ParameterTable,
    pub remaps: Vec<RemapRule>,
    pub scheduling: Option<SchedulingSelector>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSelector {
    pub package: String,
    pub executable: String,
    pub instance: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulingSelector {
    pub context: String,
    pub priority: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulingConfig {
    pub contexts: Vec<SchedContextConfig>,
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointMapping {
    pub instance: String,
    /// ROS manifest endpoint ID selected when name/type/role matching is ambiguous.
    pub manifest_endpoint: String,
    pub source_entity: Option<String>,
    pub source_callback: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildConfig {
    pub profile: String,
    pub features: Vec<String>,
}
