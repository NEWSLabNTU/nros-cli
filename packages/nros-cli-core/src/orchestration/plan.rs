use serde::{Deserialize, Serialize};

use super::schema::{
    DeadlinePolicy, InterfaceRef, ParameterTable, QosProfile, RemapRule, SchedClass, SourceLocation,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NrosPlan {
    pub version: u32,
    pub system: String,
    pub trace: PlanTrace,
    pub components: Vec<PlanComponent>,
    pub instances: Vec<PlanInstance>,
    pub interfaces: Vec<PlanInterface>,
    pub sched_contexts: Vec<PlanSchedContext>,
    pub build: PlanBuildOptions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanTrace {
    pub system_config: String,
    pub launch_record: String,
    pub generated_by: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanComponent {
    pub id: String,
    pub package: String,
    pub component: String,
    pub language: String,
    pub source_metadata: String,
    pub component_config: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanInstance {
    pub id: String,
    pub component: String,
    pub package: String,
    pub executable: String,
    pub launch_name: String,
    pub namespace: String,
    pub remaps: Vec<RemapRule>,
    pub nodes: Vec<PlanNode>,
    pub callbacks: Vec<PlanCallback>,
    pub parameters: Vec<PlanParameter>,
    pub sched_bindings: Vec<PlanSchedBinding>,
    pub trace: InstanceTrace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceTrace {
    pub launch_record_entity: String,
    pub source_metadata: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanNode {
    pub id: String,
    pub source_node: String,
    pub resolved_name: String,
    pub namespace: String,
    pub entities: Vec<PlanEntity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanEntity {
    Publisher {
        id: String,
        source_entity: String,
        resolved_name: String,
        interface: InterfaceRef,
        qos: QosProfile,
        trace: EntityTrace,
    },
    Subscriber {
        id: String,
        source_entity: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        callback: Option<String>,
        resolved_name: String,
        interface: InterfaceRef,
        qos: QosProfile,
        trace: EntityTrace,
    },
    Timer {
        id: String,
        source_entity: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        callback: Option<String>,
        period_ms: u64,
        trace: EntityTrace,
    },
    ServiceServer {
        id: String,
        source_entity: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        callback: Option<String>,
        resolved_name: String,
        interface: InterfaceRef,
        qos: Option<QosProfile>,
        trace: EntityTrace,
    },
    ServiceClient {
        id: String,
        source_entity: String,
        resolved_name: String,
        interface: InterfaceRef,
        qos: Option<QosProfile>,
        trace: EntityTrace,
    },
    ActionServer {
        id: String,
        source_entity: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        callback: Option<String>,
        resolved_name: String,
        interface: InterfaceRef,
        qos: Option<QosProfile>,
        trace: EntityTrace,
    },
    ActionClient {
        id: String,
        source_entity: String,
        resolved_name: String,
        interface: InterfaceRef,
        qos: Option<QosProfile>,
        trace: EntityTrace,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityTrace {
    pub source_artifact: SourceLocation,
    pub manifest_endpoint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanCallback {
    pub id: String,
    pub source_callback: String,
    pub group: String,
    pub sched_context: String,
    pub source: SourceLocation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanParameter {
    pub node: String,
    pub name: String,
    pub value: super::schema::ParameterValue,
    pub source: ParameterSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParameterSource {
    pub kind: ParameterSourceKind,
    pub artifact: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterSourceKind {
    SourceDefault,
    ComponentConfig,
    SystemOverlay,
    Launch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanSchedBinding {
    pub callback: String,
    pub context: String,
    pub priority: Option<u8>,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanInterface {
    pub id: String,
    pub interface: InterfaceRef,
    pub used_by: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanSchedContext {
    pub id: String,
    pub executor: String,
    /// Schema-level class; generated code maps this to the runtime scheduler class.
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanBuildOptions {
    pub target: String,
    pub board: String,
    pub rmw: String,
    pub profile: String,
    pub features: Vec<String>,
    pub cfg: ParameterTable,
}
