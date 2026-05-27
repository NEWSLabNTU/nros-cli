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
        #[serde(default)]
        callback: Option<String>,
        resolved_name: String,
        interface: InterfaceRef,
        qos: QosProfile,
        trace: EntityTrace,
    },
    Timer {
        id: String,
        source_entity: String,
        #[serde(default)]
        callback: Option<String>,
        period_ms: u64,
        trace: EntityTrace,
    },
    ServiceServer {
        id: String,
        source_entity: String,
        #[serde(default)]
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
        #[serde(default)]
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

/// Phase 173.5 — physical transport a `[[transport]]` entry selects.
/// The kind always comes from `nros.toml`; the per-kind value (ip /
/// baudrate / device) lands wherever that platform's net stack reads it
/// (board `Config` for `NanoRosOwned`, an RTOS config fragment for
/// `RtosOwned` — see [`super::generate`] / Phase 173.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Ethernet,
    Serial,
    Can,
}

impl TransportKind {
    /// The board crate Cargo feature that enables this transport.
    pub fn cargo_feature(self) -> &'static str {
        match self {
            TransportKind::Ethernet => "ethernet",
            TransportKind::Serial => "serial",
            TransportKind::Can => "can",
        }
    }
}

/// Phase 173.5 — one transport⟷RMW binding from `nros.toml`'s
/// `[[transport]]` array. Two or more entries put the build in **bridge
/// mode** (each transport runs its own RMW session;
/// `Executor::open_multi` consumes the resulting `SessionSpec`s).
///
/// `rmw`/`locator` are optional per entry; when absent they fall back to
/// the top-level `build.rmw` / the platform default. The generator —
/// not hand-written code — turns these into the board transport
/// feature(s), the per-transport `Config` values, and the RMW deps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanTransport {
    pub kind: TransportKind,
    /// IPv4 CIDR (`"10.0.2.50/24"`) or `"dhcp"` — ethernet only.
    pub ip: Option<String>,
    /// Ethernet MAC (`"02:00:00:00:00:01"`) — ethernet only. `None` ⇒
    /// the board's fixed/fused MAC. (Phase 172.J — replaces
    /// `config.toml`'s `[network].mac`.)
    #[serde(default)]
    pub mac: Option<String>,
    /// Default IPv4 gateway (`"10.0.2.2"`) — ethernet only. `None` ⇒ a
    /// flat link with no gateway. (Phase 172.J — replaces
    /// `config.toml`'s `[network].gateway`.)
    #[serde(default)]
    pub gateway: Option<String>,
    /// Device handle (`"UART0"`, `"CAN0"`) — serial / can only.
    pub device: Option<String>,
    /// Line rate (serial baud / CAN bitrate) — serial / can only.
    pub baudrate: Option<u32>,
    /// RMW that rides this transport. `None` ⇒ inherit `build.rmw`.
    pub rmw: Option<String>,
    /// Zenoh/DDS locator seeding this transport's session. `None` ⇒
    /// platform / env default.
    pub locator: Option<String>,
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
    /// Phase 173.5 — `nros.toml` `[[transport]]` entries. Empty ⇒
    /// zero-config single-transport build (board default transport +
    /// the single linked RMW). Defaulted so pre-173.5 plans parse.
    #[serde(default)]
    pub transports: Vec<PlanTransport>,
}

impl PlanBuildOptions {
    /// `true` when more than one transport is declared — the build runs
    /// multiple RMW sessions via `Executor::open_multi` (bridge mode).
    pub fn is_bridge(&self) -> bool {
        self.transports.len() > 1
    }

    /// Validate the `[[transport]]` array against per-kind field rules.
    /// Returns the list of human-readable problems (empty ⇒ valid) so
    /// the caller can surface them all at once rather than one per run.
    pub fn validate_transports(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (i, t) in self.transports.iter().enumerate() {
            let at = format!("transport[{i}] (kind = {:?})", t.kind);
            match t.kind {
                TransportKind::Ethernet => {
                    if t.device.is_some() || t.baudrate.is_some() {
                        problems.push(format!("{at}: `device`/`baudrate` are serial/can-only"));
                    }
                }
                TransportKind::Serial | TransportKind::Can => {
                    if t.ip.is_some() {
                        problems.push(format!("{at}: `ip` is ethernet-only"));
                    }
                    if t.mac.is_some() {
                        problems.push(format!("{at}: `mac` is ethernet-only"));
                    }
                    if t.gateway.is_some() {
                        problems.push(format!("{at}: `gateway` is ethernet-only"));
                    }
                }
            }
        }
        problems
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    fn build_with(transports_json: &str) -> PlanBuildOptions {
        let json = format!(
            r#"{{
                "target": "thumbv7m-none-eabi",
                "board": "baremetal",
                "rmw": "zenoh",
                "profile": "release",
                "features": [],
                "cfg": {{}}{transports_json}
            }}"#
        );
        serde_json::from_str(&json).expect("PlanBuildOptions parses")
    }

    #[test]
    fn pre_173_5_plan_without_transports_parses_to_empty() {
        let build = build_with("");
        assert!(build.transports.is_empty());
        assert!(!build.is_bridge());
        assert!(build.validate_transports().is_empty());
    }

    #[test]
    fn single_ethernet_transport_parses_and_validates() {
        let build = build_with(
            r#",
            "transports": [
                { "kind": "ethernet", "ip": "10.0.2.50/24", "rmw": "zenoh", "locator": "tcp/10.0.2.2:7447" }
            ]"#,
        );
        assert_eq!(build.transports.len(), 1);
        assert!(!build.is_bridge());
        assert_eq!(build.transports[0].kind, TransportKind::Ethernet);
        assert_eq!(build.transports[0].kind.cargo_feature(), "ethernet");
        assert_eq!(build.transports[0].ip.as_deref(), Some("10.0.2.50/24"));
        assert!(build.validate_transports().is_empty());
    }

    #[test]
    fn two_transports_are_bridge_mode() {
        let build = build_with(
            r#",
            "transports": [
                { "kind": "ethernet", "ip": "dhcp", "rmw": "zenoh" },
                { "kind": "serial", "device": "UART0", "baudrate": 115200, "rmw": "cyclonedds" }
            ]"#,
        );
        assert!(build.is_bridge());
        assert_eq!(build.transports[1].kind.cargo_feature(), "serial");
        assert_eq!(build.transports[1].baudrate, Some(115200));
        assert!(build.validate_transports().is_empty());
    }

    #[test]
    fn mismatched_transport_fields_are_reported() {
        // ethernet with a baudrate, serial with an ip — both wrong.
        let build = build_with(
            r#",
            "transports": [
                { "kind": "ethernet", "baudrate": 9600 },
                { "kind": "serial", "ip": "10.0.0.1/24", "device": "UART0" }
            ]"#,
        );
        let problems = build.validate_transports();
        assert_eq!(problems.len(), 2, "both mismatches reported: {problems:?}");
    }

    #[test]
    fn ethernet_mac_and_gateway_parse_and_validate() {
        // Phase 172.J — mac + gateway on an ethernet transport.
        let build = build_with(
            r#",
            "transports": [
                { "kind": "ethernet", "ip": "10.0.2.50/24",
                  "mac": "02:00:00:00:00:01", "gateway": "10.0.2.2" }
            ]"#,
        );
        assert_eq!(
            build.transports[0].mac.as_deref(),
            Some("02:00:00:00:00:01")
        );
        assert_eq!(build.transports[0].gateway.as_deref(), Some("10.0.2.2"));
        assert!(build.validate_transports().is_empty());
    }

    #[test]
    fn mac_and_gateway_are_ethernet_only() {
        // Phase 172.J — serial transport rejects mac + gateway.
        let build = build_with(
            r#",
            "transports": [
                { "kind": "serial", "device": "UART0", "baudrate": 115200,
                  "mac": "02:00:00:00:00:01", "gateway": "10.0.2.2" }
            ]"#,
        );
        let problems = build.validate_transports();
        assert_eq!(
            problems.len(),
            2,
            "mac + gateway both rejected: {problems:?}"
        );
    }

    #[test]
    fn unknown_transport_kind_is_rejected() {
        let json = r#"{
            "target": "x", "board": "native", "rmw": "zenoh",
            "profile": "release", "features": [], "cfg": {},
            "transports": [ { "kind": "bluetooth" } ]
        }"#;
        assert!(serde_json::from_str::<PlanBuildOptions>(json).is_err());
    }
}
