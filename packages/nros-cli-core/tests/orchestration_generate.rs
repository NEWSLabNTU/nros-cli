use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use nros_cli_core::orchestration::generate::{generate_package, GenerateOptions};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("orchestration")
        .join(name)
}

fn temp_output(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nros_cli_core_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn generate_fixture(name: &str, plan_fixture: &str) -> PathBuf {
    let output_dir = temp_output(name);
    generate_plan(name, fixture(plan_fixture), output_dir.clone());
    output_dir
}

fn generate_plan(name: &str, plan_path: PathBuf, output_dir: PathBuf) {
    generate_package(&GenerateOptions {
        package_name: "nros-generated-test".to_string(),
        output_dir,
        plan_path,
        nros_path: PathBuf::from("/workspace/packages/core/nros"),
        nros_orchestration_path: PathBuf::from("/workspace/packages/core/nros-orchestration"),
        component_workspace: None,
    })
    .unwrap_or_else(|error| panic!("{name} generated package writes: {error:?}"));
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("repo root ancestor")
        .to_path_buf()
}

fn generate_workspace_backed_fixture(name: &str, plan_fixture: &str) -> PathBuf {
    let output_dir = temp_output(name);
    let root = workspace_root();
    generate_package(&GenerateOptions {
        package_name: "nros-generated-test".to_string(),
        output_dir: output_dir.clone(),
        plan_path: fixture(plan_fixture),
        nros_path: root.join("packages/core/nros"),
        nros_orchestration_path: root.join("packages/core/nros-orchestration"),
        component_workspace: None,
    })
    .unwrap_or_else(|error| panic!("{name} generated package writes: {error:?}"));
    output_dir
}

#[test]
fn generated_package_writes_manifest_build_script_and_main() {
    let output_dir = generate_fixture("generated_package_writes_files", "plan_pub_sub.json");

    let cargo_toml = fs::read_to_string(output_dir.join("Cargo.toml")).expect("read Cargo.toml");
    assert!(cargo_toml.contains("name = \"nros-generated-test\""));
    // Phase 126.M4 — per-RMW `rmw-*-cffi` feature names dropped (Phase
    // 128.C); generator now emits a single `nros/rmw-cffi` umbrella.
    assert!(cargo_toml.contains(
        "default = [\"std\", \"nros/platform-posix\", \"nros/rmw-cffi\", \"nros-orchestration/rmw-cffi\"]"
    ));
    assert!(cargo_toml.contains("nros = { path = \"/workspace/packages/core/nros\""));
    assert!(cargo_toml
        .contains("nros-orchestration = { path = \"/workspace/packages/core/nros-orchestration\""));
    assert!(cargo_toml.contains("nros-platform-cffi = { path = \"/workspace/packages/core/nros-platform-cffi\", default-features = false, features = [\"posix-c-port\"] }"));
    assert!(!cargo_toml.contains("nros-cli-core"));
    assert!(!cargo_toml.contains("serde_json"));

    let build_rs = fs::read_to_string(output_dir.join("build.rs")).expect("read build.rs");
    assert!(build_rs.contains("const PLAN_PATH: &str ="));
    assert!(build_rs.contains("// Generated from: "));
    assert!(build_rs.contains("pub const CALLBACK_COUNT: usize = 2;"));
    assert!(build_rs.contains("pub const SCHED_CONTEXT_COUNT: usize = 1;"));
    assert!(build_rs.contains("pub static COMPONENTS: [ComponentSpec; 2]"));
    assert!(build_rs.contains("pub static INSTANCES: [InstanceSpec; 2]"));
    assert!(build_rs.contains("pub static NODES: [NodeSpec; 2]"));
    assert!(build_rs.contains("pub static PARAMETERS: [ParameterSpec; 1]"));
    assert!(build_rs.contains("pub static SCHED_CONTEXTS: [SchedContextSpec; 1]"));
    assert!(build_rs.contains("pub static CALLBACK_BINDINGS: [CallbackBindingSpec; 2]"));
    assert!(build_rs.contains("pub static SYSTEM: SystemSpec"));
    assert!(build_rs.contains("GeneratedNodeRuntime"));
    assert!(build_rs.contains("register_component::<demo_nodes_rs::talker::Component>"));
    assert!(build_rs.contains("register_component::<demo_nodes_rs::listener::Component>"));
    assert!(build_rs.contains("PlanId("));
    assert!(build_rs.contains("SchedClassSpec::Fifo"));
    assert!(build_rs.contains("PrioritySpec::BestEffort"));
    assert!(build_rs.contains("deadline_policy: DeadlinePolicySpec::Activated"));
    assert!(build_rs.contains("pub fn register_backends()"));
    assert!(build_rs.contains("nros_rmw_zenoh::register()"));
    assert!(build_rs.contains("instantiate_callback_handles"));
    assert!(build_rs.contains("handles.set("));
    assert!(!build_rs.contains("serde_json"));
    assert!(!build_rs.contains("nros_cli_core"));

    let main_rs = fs::read_to_string(output_dir.join("src/main.rs")).expect("read main.rs");
    assert!(main_rs.contains("nros_generated::register_backends();"));
    assert!(main_rs.contains("Executor::open"));
    assert!(main_rs.contains("#[cfg(feature = \"std\")]"));
    assert!(main_rs.contains("ExecutorConfig::from_env()"));
    assert!(main_rs.contains("#[cfg(not(feature = \"std\"))]"));
    assert!(main_rs.contains("ExecutorConfig::default_const()"));
    assert!(main_rs.contains("create_sched_context(spec.to_nros_node())"));
    assert!(main_rs.contains("instantiate_components"));
    assert!(main_rs.contains("bind_handle_to_sched_context"));
    assert!(main_rs.contains("spin_blocking(SpinOptions::default())"));
    assert!(main_rs.contains("spin_default()"));
    // Phase 173.2b — native/posix is the hosted `HostedMain` shape: a
    // plain `fn main() -> Result<..>`, no `#![no_std]` and no board
    // `run()` entry leaking in from a bare-metal platform.
    assert!(main_rs.contains("fn main() -> core::result::Result<(), nros::NodeError> {"));
    assert!(!main_rs.contains("#![no_std]"));
    assert!(!main_rs.contains("::run("));
}

#[test]
fn generated_package_features_follow_rtos_plan() {
    let root = temp_output("generated_package_features_follow_rtos_plan");
    fs::create_dir_all(&root).expect("create temp plan dir");
    let plan_path = root.join("nros-plan.json");
    let plan = include_str!("fixtures/orchestration/plan_pub_sub.json")
        .replace(
            "\"target\": \"x86_64-unknown-linux-gnu\"",
            "\"target\": \"thumbv7em-none-eabihf\"",
        )
        .replace("\"board\": \"native\"", "\"board\": \"zephyr\"")
        .replace("\"rmw\": \"zenoh\"", "\"rmw\": \"xrce\"")
        .replace("\"rmw-zenoh\"", "\"rmw-xrce\"");
    fs::write(&plan_path, plan).expect("write RTOS plan");

    let output_dir = root.join("generated");
    generate_plan(
        "generated_package_features_follow_rtos_plan",
        plan_path,
        output_dir.clone(),
    );
    let cargo_toml = fs::read_to_string(output_dir.join("Cargo.toml")).expect("read Cargo.toml");

    assert!(cargo_toml.contains(
        "default = [\"nros/platform-zephyr\", \"platform-zephyr\", \"nros/rmw-cffi\", \"nros-orchestration/rmw-cffi\"]"
    ));
    assert!(!cargo_toml.contains("\"std\""));
    assert!(!cargo_toml.contains("platform-posix"));
    assert!(!cargo_toml.contains("nros-platform-cffi"));
}

#[test]
fn declared_serial_transport_selects_board_feature() {
    // Phase 173.5 — a `[[transport]]` entry drives the board crate's
    // transport feature. A bare-metal board + a single serial transport
    // ⇒ the board dep disables defaults and selects `serial` (swapping
    // off the board's default `ethernet`).
    let root = temp_output("declared_serial_transport_selects_board_feature");
    fs::create_dir_all(&root).expect("create temp plan dir");
    let plan_path = root.join("nros-plan.json");
    let plan = include_str!("fixtures/orchestration/plan_pub_sub.json")
        .replace(
            "\"target\": \"x86_64-unknown-linux-gnu\"",
            "\"target\": \"thumbv7m-none-eabi\"",
        )
        .replace("\"board\": \"native\"", "\"board\": \"baremetal\"")
        .replace(
            "\"cfg\": {}",
            "\"cfg\": {}, \"transports\": [{ \"kind\": \"serial\", \"device\": \"UART0\", \"baudrate\": 115200 }]",
        );
    fs::write(&plan_path, plan).expect("write transport plan");

    let output_dir = root.join("generated");
    generate_plan(
        "declared_serial_transport_selects_board_feature",
        plan_path,
        output_dir.clone(),
    );

    let cargo_toml = fs::read_to_string(output_dir.join("Cargo.toml")).expect("read Cargo.toml");
    assert!(
        cargo_toml.contains(
            "nros-board-mps2-an385 = { path = \"/workspace/packages/boards/nros-board-mps2-an385\", default-features = false, features = [\"serial\"] }"
        ),
        "serial transport selects the board `serial` feature with defaults off:\n{cargo_toml}"
    );
}

#[test]
fn generated_package_wires_freertos_entry() {
    let root = temp_output("generated_package_wires_freertos_entry");
    fs::create_dir_all(&root).expect("create temp plan dir");
    let plan_path = root.join("nros-plan.json");
    let plan = include_str!("fixtures/orchestration/plan_pub_sub.json")
        .replace(
            "\"target\": \"x86_64-unknown-linux-gnu\"",
            "\"target\": \"thumbv7m-none-eabi\"",
        )
        .replace("\"board\": \"native\"", "\"board\": \"freertos\"");
    fs::write(&plan_path, plan).expect("write FreeRTOS plan");

    let output_dir = root.join("generated");
    generate_plan(
        "generated_package_wires_freertos_entry",
        plan_path,
        output_dir.clone(),
    );

    let cargo_toml = fs::read_to_string(output_dir.join("Cargo.toml")).expect("read Cargo.toml");
    assert!(cargo_toml.contains(
        "default = [\"nros/platform-freertos\", \"platform-freertos\", \"nros/rmw-cffi\", \"nros-orchestration/rmw-cffi\"]"
    ));
    assert!(cargo_toml.contains("nros-board-mps2-an385-freertos"));
    assert!(cargo_toml.contains("panic-semihosting"));

    let cargo_config =
        fs::read_to_string(output_dir.join(".cargo/config.toml")).expect("read cargo config");
    assert!(cargo_config.contains("[target.thumbv7m-none-eabi]"));
    assert!(cargo_config.contains("mps2_an385.ld"));

    let main_rs = fs::read_to_string(output_dir.join("src/main.rs")).expect("read main.rs");
    // Phase 173.2b collapsed the per-platform `#[cfg(feature = ...)]` entry
    // blocks into one shape chosen by `profile().board_entry`. FreeRTOS is a
    // bare-metal `BoardRun`, so the generated `main.rs` is unconditional
    // `#![no_std]` / `#![no_main]` with a single `_start` entry that drives
    // the board crate's `run()` — no `cfg(feature = "platform-freertos")`
    // gate survives.
    assert!(main_rs.contains("#![no_std]"));
    assert!(main_rs.contains("#![no_main]"));
    assert!(main_rs.contains("use panic_semihosting as _;"));
    assert!(main_rs.contains("extern \"C\" fn _start() -> !"));
    assert!(main_rs.contains("nros_board_mps2_an385_freertos::run("));
    assert!(main_rs.contains("nros_board_mps2_an385_freertos::Config::default()"));
    // Single shape: no other platform's entry leaks in.
    assert!(!main_rs.contains("ExecutorConfig::from_env()"));
    assert!(!main_rs.contains("esp_hal::main"));
}

#[test]
fn generated_package_registers_service_and_action_callbacks() {
    let output_dir = generate_fixture(
        "generated_package_registers_service_and_action_callbacks",
        "plan_service_action.json",
    );
    let build_rs = fs::read_to_string(output_dir.join("build.rs")).expect("read build.rs");

    assert!(build_rs.contains("pub const CALLBACK_COUNT: usize = 2;"));
    assert!(build_rs.contains("noop_raw_service"));
    assert!(build_rs.contains("noop_raw_goal"));
    assert!(build_rs.contains("noop_raw_cancel"));
    assert!(build_rs.contains("noop_raw_accepted"));
    assert!(build_rs.contains("register_service_raw_sized_on::<1024, 1024>"));
    assert!(build_rs.contains("register_action_server_raw_sized_on::<1024, 1024, 1024, 4>"));
    assert!(build_rs.contains("action_1.handle_id()"));
    assert!(!build_rs.contains("unsupported generated callback"));
}

#[test]
fn generated_service_action_package_is_readable_by_cargo_metadata() {
    let output_dir = generate_workspace_backed_fixture(
        "generated_service_action_package_is_readable_by_cargo_metadata",
        "plan_service_action.json",
    );
    let manifest_path = output_dir.join("Cargo.toml");

    let output = Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .output()
        .expect("run cargo metadata for generated service/action package");

    assert!(
        output.status.success(),
        "cargo metadata failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn generated_package_is_readable_by_cargo_metadata() {
    let output_dir =
        generate_workspace_backed_fixture("generated_package_cargo_metadata", "plan_pub_sub.json");
    let manifest_path = output_dir.join("Cargo.toml");

    let output = Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .output()
        .expect("run cargo metadata for generated package");

    assert!(
        output.status.success(),
        "cargo metadata failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"name\":\"nros-generated-test\""));
    assert!(stdout.contains("\"src_path\""));
}

#[test]
fn generated_package_output_is_stable() {
    let output_dir = generate_fixture("generated_package_output_is_stable", "plan_pub_sub.json");
    let first_cargo = fs::read_to_string(output_dir.join("Cargo.toml")).expect("read Cargo.toml");
    let first_build = fs::read_to_string(output_dir.join("build.rs")).expect("read build.rs");
    let first_main = fs::read_to_string(output_dir.join("src/main.rs")).expect("read main.rs");

    generate_package(&GenerateOptions {
        package_name: "nros-generated-test".to_string(),
        output_dir: output_dir.clone(),
        plan_path: fixture("plan_pub_sub.json"),
        nros_path: PathBuf::from("/workspace/packages/core/nros"),
        nros_orchestration_path: PathBuf::from("/workspace/packages/core/nros-orchestration"),
        component_workspace: None,
    })
    .expect("second generated package write");

    assert_eq!(
        first_cargo,
        fs::read_to_string(output_dir.join("Cargo.toml")).expect("reread Cargo.toml")
    );
    assert_eq!(
        first_build,
        fs::read_to_string(output_dir.join("build.rs")).expect("reread build.rs")
    );
    assert_eq!(
        first_main,
        fs::read_to_string(output_dir.join("src/main.rs")).expect("reread main.rs")
    );
}

#[test]
fn generated_tables_cover_multiple_instances_of_same_component() {
    let output_dir = generate_fixture(
        "generated_tables_multi_instance",
        "plan_multi_instance.json",
    );
    let build_rs = fs::read_to_string(output_dir.join("build.rs")).expect("read build.rs");

    assert!(build_rs.contains("pub const CALLBACK_COUNT: usize = 2;"));
    assert!(build_rs.contains("pub static COMPONENTS: [ComponentSpec; 1]"));
    assert!(build_rs.contains("pub static INSTANCES: [InstanceSpec; 2]"));
    assert!(build_rs.contains("pub static PARAMETERS: [ParameterSpec; 2]"));
    assert!(build_rs.contains("left_talker"));
    assert!(build_rs.contains("right_talker"));
    assert!(build_rs.contains("/left/talker"));
    assert!(build_rs.contains("/right/talker"));
    assert!(build_rs.contains("parameter_start: 0, parameter_len: 1"));
    assert!(build_rs.contains("parameter_start: 1, parameter_len: 1"));
    assert!(build_rs.contains("value: ParameterValue::I64(5)"));
    assert!(build_rs.contains("value: ParameterValue::I64(2)"));
    assert!(build_rs.contains("CallbackBindingSpec { callback_index: 0, sched_context_index: 1 }"));
    assert!(build_rs.contains("CallbackBindingSpec { callback_index: 1, sched_context_index: 1 }"));
}
