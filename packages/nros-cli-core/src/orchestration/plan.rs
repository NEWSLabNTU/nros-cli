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
    /// Phase 172.B — callback execution chains inferred from the topic
    /// dataflow graph (publisher topic → subscriber callback). Additive; old
    /// plans (v1) omit it and deserialize to an empty vec. Omitted from output
    /// when empty so chain-less plans stay byte-identical to v1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callback_chains: Vec<PlanCallbackChain>,
    /// Phase 172.C — callback groups derived from the chains (one
    /// mutually-exclusive group per chain; one reentrant singleton group per
    /// chain-less callback). Additive; old plans omit it and deserialize to
    /// an empty vec. Omitted from output when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callback_groups: Vec<PlanCallbackGroup>,
    /// Phase 172.A — managed-lifecycle (REP-2002) spec for the generated
    /// binary's node. Additive; absent ⇒ plain node (pre-172.A). Omitted from
    /// output when absent so non-lifecycle plans stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<PlanLifecycle>,
    /// Phase 172.I — named shared-memory regions that co-located components in
    /// one generated binary can read/write (a critical-section-guarded byte
    /// blackboard; components own the typed view). Additive; absent ⇒ no shared
    /// state. Omitted from output when empty so plans stay byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_state: Vec<PlanSharedRegion>,
    /// Phase 172.H — runtime parameter-override persistence backend. Additive;
    /// absent ⇒ no persistence (generated runtime keeps no param services).
    /// Omitted from output when absent so plans stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_persistence: Option<PlanParamPersistence>,
    pub build: PlanBuildOptions,
}

/// Phase 172.H — where the generated runtime persists parameter overrides set
/// after boot, so they survive a restart. `backend` selects the store kind
/// (only `"file"`, a hosted text file, today); `path` is its location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanParamPersistence {
    pub backend: String,
    pub path: String,
}

/// Phase 172.I — one named shared-memory region. `bytes` sizes a
/// critical-section-guarded byte region; a component reads/writes it through
/// the generated accessor and overlays its own typed view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanSharedRegion {
    pub id: String,
    pub bytes: usize,
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

/// Phase 172.B — an inferred (or overridden) callback execution chain: an
/// ordered sequence of callbacks where each consumes the topic the previous
/// produced. The head is a chain entry (a timer, or a subscriber whose topic
/// has no in-system publisher); `links` records the producing topic for each
/// edge so the chain is auditable. 172.C derives callback groups from these
/// chains; 172.G assigns tiers per chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanCallbackChain {
    /// Stable chain id, e.g. `chain/<head-callback-id>`.
    pub id: String,
    /// Ordered callback ids from head to tail.
    pub callbacks: Vec<String>,
    /// One entry per edge between consecutive `callbacks`.
    pub links: Vec<PlanChainLink>,
    /// `true` when the planner inferred this chain from the topic graph;
    /// `false` when it came from an explicit `[[chain]]` override.
    pub inferred: bool,
}

/// One dataflow edge in a [`PlanCallbackChain`]: `from` publishes `topic`,
/// which `to` subscribes to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanChainLink {
    pub from: String,
    pub to: String,
    pub topic: String,
}

/// Phase 172.C — dispatch concurrency class of a [`PlanCallbackGroup`],
/// mirroring rclcpp's two callback-group kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallbackGroupKind {
    /// Members never run concurrently with one another (serialized
    /// dispatch) — the safe default for dataflow-coupled pipeline stages
    /// that may share state.
    MutuallyExclusive,
    /// Members may run concurrently — inferred for callbacks with no
    /// detected dataflow coupling.
    Reentrant,
}

/// Phase 172.C — an inferred (or overridden) callback group. Each callback
/// belongs to exactly one group; the group's [`CallbackGroupKind`] decides
/// whether its members serialize or may run concurrently. Derived from the
/// 172.B callback chains: every chain becomes one mutually-exclusive group
/// (its stages serialize), and every callback outside any chain becomes its
/// own reentrant group (no coupling detected → concurrent-safe). 172.G
/// assigns scheduling tiers on top of this grouping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanCallbackGroup {
    /// Stable group id, e.g. `group/<chain-head>` or `group/<callback-id>`.
    pub id: String,
    /// Serialize vs concurrent dispatch.
    pub kind: CallbackGroupKind,
    /// Callback ids that belong to this group (chain order for chain
    /// groups; a single callback for reentrant groups).
    pub callbacks: Vec<String>,
    /// `true` when the planner inferred this group from the chains;
    /// `false` when it came from an explicit `[[group]]` override.
    pub inferred: bool,
}

/// Phase 172.A — boot autostart policy for a managed-lifecycle (REP-2002) node.
/// The generated runtime registers the five `~/change_state` / `~/get_state`
/// services and then drives the node to this state at boot; `ros2 lifecycle`
/// can drive it further at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleAutostart {
    /// Register the services but leave the node `Unconfigured` — every
    /// transition is externally driven (`ros2 lifecycle set`).
    None,
    /// Auto-`configure` to `Inactive` at boot.
    Configure,
    /// Auto-`configure` then `activate` to `Active` at boot.
    Active,
}

/// Phase 172.A — managed-lifecycle spec for the generated binary. Its presence
/// marks the binary's node as managed; absence keeps the pre-172.A behaviour
/// (a plain node brought up once at boot). The runtime models one lifecycle
/// state machine per executor, so this is currently system-level; per-instance
/// (multiple managed nodes in one binary) is a deferred runtime extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanLifecycle {
    /// State the generated runtime drives the node to at boot.
    pub autostart: LifecycleAutostart,
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
    Wifi,
    Serial,
    Can,
}

impl TransportKind {
    /// The board crate Cargo feature that enables this transport.
    pub fn cargo_feature(self) -> &'static str {
        match self {
            TransportKind::Ethernet => "ethernet",
            TransportKind::Wifi => "wifi",
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
    /// Stable transport id used to bind a node/instance to this session
    /// (`SystemComponent.transport`). `None` ⇒ defaults to `rmw` (works when
    /// each transport has a distinct rmw). Phase 172.K.
    #[serde(default)]
    pub id: Option<String>,
    /// IPv4 CIDR (`"10.0.2.50/24"`) or `"dhcp"` — ethernet/wifi only.
    pub ip: Option<String>,
    /// WiFi SSID — wifi only. Phase 172.K.
    #[serde(default)]
    pub ssid: Option<String>,
    /// WiFi password — wifi only. Phase 172.K.
    #[serde(default)]
    pub password: Option<String>,
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
    /// the single linked RMW). Defaulted so pre-173.5 plans parse;
    /// skip-when-empty so the stable pretty fixtures (zero-config
    /// builds) round-trip without an empty `"transports": []`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
                    if t.ssid.is_some() || t.password.is_some() {
                        problems.push(format!("{at}: `ssid`/`password` are wifi-only"));
                    }
                }
                TransportKind::Wifi => {
                    // wifi carries ssid/password (+ optional static ip/gateway);
                    // mac is ethernet-only, device/baudrate are serial/can-only.
                    if t.device.is_some() || t.baudrate.is_some() {
                        problems.push(format!("{at}: `device`/`baudrate` are serial/can-only"));
                    }
                    if t.mac.is_some() {
                        problems.push(format!("{at}: `mac` is ethernet-only"));
                    }
                }
                TransportKind::Serial | TransportKind::Can => {
                    if t.ip.is_some() {
                        problems.push(format!("{at}: `ip` is ethernet/wifi-only"));
                    }
                    if t.mac.is_some() {
                        problems.push(format!("{at}: `mac` is ethernet-only"));
                    }
                    if t.gateway.is_some() {
                        problems.push(format!("{at}: `gateway` is ethernet/wifi-only"));
                    }
                    if t.ssid.is_some() || t.password.is_some() {
                        problems.push(format!("{at}: `ssid`/`password` are wifi-only"));
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
    fn wifi_transport_parses_with_ssid_password_and_id() {
        // Phase 172.K.4 — wifi kind + ssid/password + transport id.
        let build = build_with(
            r#",
            "transports": [
                { "kind": "wifi", "id": "wlan", "ssid": "Net", "password": "pw",
                  "ip": "10.0.0.50/24", "rmw": "zenoh" }
            ]"#,
        );
        assert_eq!(build.transports[0].kind, TransportKind::Wifi);
        assert_eq!(build.transports[0].kind.cargo_feature(), "wifi");
        assert_eq!(build.transports[0].id.as_deref(), Some("wlan"));
        assert_eq!(build.transports[0].ssid.as_deref(), Some("Net"));
        assert!(build.validate_transports().is_empty());
    }

    #[test]
    fn ssid_password_are_wifi_only() {
        // Phase 172.K.4 — ethernet + serial reject ssid/password.
        let build = build_with(
            r#",
            "transports": [
                { "kind": "ethernet", "ssid": "Net" },
                { "kind": "serial", "device": "UART0", "password": "pw" }
            ]"#,
        );
        let problems = build.validate_transports();
        assert_eq!(problems.len(), 2, "ssid + password rejected: {problems:?}");
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
