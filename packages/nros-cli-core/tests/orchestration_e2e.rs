use std::{
    fs,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
    time::{SystemTime, UNIX_EPOCH},
};

use nros_cli_core::cmd::{build, check, metadata, plan};
use nros_cli_core::orchestration::{
    plan::{NrosPlan, PlanComponent, PlanEntity},
    schema::ParameterValue,
};
use serde_json::Value;

#[test]
fn fixture_workspace_plans_checks_and_builds_generated_package() {
    let fixture = fixture_workspace();
    let output = temp_output("orchestration_e2e");
    let out_dir = output.join("build/e2e_system/nros");
    let generated_dir = out_dir.join("generated");
    let demo_pkg = fixture.join("src/demo_pkg");

    metadata::run(metadata::Args {
        system_pkg: "e2e_system".to_string(),
        workspace: Some(fixture.clone()),
        out_dir: Some(out_dir.clone()),
        metadata: vec![fixture.join("artifacts/talker.metadata.json")],
    })
    .expect("metadata command preserves fixture source metadata");

    plan::run(plan::Args {
        system_pkg: "e2e_system".to_string(),
        launch_file: demo_pkg.join("launch/system.launch.xml"),
        record: None,
        workspace: Some(fixture.clone()),
        out_dir: Some(out_dir.clone()),
        metadata: Vec::new(),
        manifests: vec![demo_pkg.join("manifest/system.launch.yaml")],
        nros_toml: Vec::new(),
        launch_args: Vec::new(),
    })
    .expect("plan command parses launch and writes checked artifacts");

    let plan_path = out_dir.join("nros-plan.json");
    check::run(check::Args {
        plan: plan_path.clone(),
    })
    .expect("check command validates generated plan");

    let plan: NrosPlan =
        serde_json::from_str(&fs::read_to_string(&plan_path).expect("read generated plan"))
            .expect("generated plan has canonical schema");
    assert_eq!(plan.system, "e2e_system");
    assert_eq!(plan.instances.len(), 1);
    assert_eq!(plan.instances[0].package, "demo_pkg");
    assert_eq!(plan.instances[0].parameters[0].name, "rate_hz");
    assert_eq!(
        plan.instances[0].parameters[0].value,
        ParameterValue::Integer(25)
    );

    let record: Value = serde_json::from_str(
        &fs::read_to_string(out_dir.join("record.json")).expect("read record"),
    )
    .expect("record is JSON");
    let nodes = record["node"].as_array().expect("record has node array");
    assert_eq!(nodes[0]["package"].as_str(), Some("demo_pkg"));
    assert_eq!(nodes[0]["executable"].as_str(), Some("talker"));

    build::run(build::Args {
        project: Some(fixture),
        system_plan: Some(plan_path),
        system_output: Some(generated_dir.clone()),
        system_package: Some("nros-e2e-generated".to_string()),
        nano_ros_workspace: Some(nano_ros_workspace()),
        release: false,
        target: None,
        passthrough: Vec::new(),
        launch: None,
        system_pkg: None,
        metadata: Vec::new(),
        manifest: Vec::new(),
        launch_arg: Vec::new(),
        out_dir: None,
    })
    .expect("build command compiles generated package");

    assert!(generated_dir.join("Cargo.toml").is_file());
    assert!(generated_dir.join("src/main.rs").is_file());
    for lang in ["rust", "c", "cpp"] {
        let manifest_path = out_dir.join("interfaces").join(lang).join("manifest.json");
        let manifest: Value = serde_json::from_str(
            &fs::read_to_string(&manifest_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", manifest_path.display())),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", manifest_path.display()));
        assert_eq!(
            manifest["schema"].as_str(),
            Some("nano-ros/interface-cache/v1")
        );
        assert_eq!(manifest["system"].as_str(), Some("e2e_system"));
        assert_eq!(
            manifest["interfaces"][0]["id"].as_str(),
            Some("std_msgs/msg/String")
        );
    }

    let binary = out_dir
        .join("target")
        .join(&plan.build.target)
        .join("debug")
        .join("nros-e2e-generated");
    assert!(
        binary.is_file(),
        "generated binary exists at {}",
        binary.display()
    );

    let port = free_local_port();
    let _zenohd = start_zenohd(port);
    assert_generated_binary_spins(&binary, port);

    let multi_plan_path = out_dir.join("nros-plan-multi-instance.json");
    let mut multi_plan = plan.clone();
    add_second_instance(&mut multi_plan);
    fs::write(
        &multi_plan_path,
        serde_json::to_string_pretty(&multi_plan).expect("serialize multi-instance plan"),
    )
    .expect("write multi-instance plan");
    check::run(check::Args {
        plan: multi_plan_path.clone(),
    })
    .expect("check command validates generated multi-instance plan");
    let multi_generated_dir = out_dir.join("generated-multi");
    build::run(build::Args {
        project: Some(fixture_workspace()),
        system_plan: Some(multi_plan_path),
        system_output: Some(multi_generated_dir.clone()),
        system_package: Some("nros-e2e-generated-multi".to_string()),
        nano_ros_workspace: Some(nano_ros_workspace()),
        release: false,
        target: None,
        passthrough: Vec::new(),
        launch: None,
        system_pkg: None,
        metadata: Vec::new(),
        manifest: Vec::new(),
        launch_arg: Vec::new(),
        out_dir: None,
    })
    .expect("build command compiles generated multi-instance package");
    assert!(
        multi_generated_dir
            .join("../target")
            .join(&multi_plan.build.target)
            .join("debug")
            .join("nros-e2e-generated-multi")
            .is_file()
    );
}

#[test]
fn fixture_workspace_builds_and_boots_generated_freertos_package() {
    let fixture = fixture_workspace();
    let output = temp_output("orchestration_e2e_freertos");
    let out_dir = output.join("build/e2e_system/nros");
    let generated_dir = out_dir.join("generated-freertos");
    let plan_path = out_dir.join("nros-plan-freertos.json");
    fs::create_dir_all(&out_dir).expect("create FreeRTOS output dir");

    let mut plan = fixture_plan("plan_multi_instance.json");
    retarget_plan_to_fixture_component(&mut plan);
    retarget_plan_to_freertos(&mut plan);
    fs::write(
        &plan_path,
        serde_json::to_string_pretty(&plan).expect("serialize FreeRTOS plan"),
    )
    .expect("write FreeRTOS plan");

    check::run(check::Args {
        plan: plan_path.clone(),
    })
    .expect("check command validates generated FreeRTOS plan");
    build::run(build::Args {
        project: Some(fixture),
        system_plan: Some(plan_path),
        system_output: Some(generated_dir.clone()),
        system_package: Some("nros-e2e-generated-freertos".to_string()),
        nano_ros_workspace: Some(nano_ros_workspace()),
        release: false,
        target: None,
        passthrough: Vec::new(),
        launch: None,
        system_pkg: None,
        metadata: Vec::new(),
        manifest: Vec::new(),
        launch_arg: Vec::new(),
        out_dir: None,
    })
    .expect("build command compiles generated FreeRTOS package");

    let binary = out_dir
        .join("target")
        .join("thumbv7m-none-eabi")
        .join("release")
        .join("nros-e2e-generated-freertos");
    assert!(
        binary.is_file(),
        "generated FreeRTOS binary exists at {}",
        binary.display()
    );
    assert_freertos_binary_boots(&binary);
}

#[test]
fn fixture_workspace_links_mixed_c_component_archive() {
    let fixture = fixture_workspace();
    let output = temp_output("orchestration_e2e_mixed_c");
    let out_dir = output.join("build/e2e_system/nros");
    let generated_dir = out_dir.join("generated-mixed-c");
    let plan_path = out_dir.join("nros-plan-mixed-c.json");
    fs::create_dir_all(&out_dir).expect("create mixed C output dir");

    let archive = build_native_counter_archive(&output, "c_counter", "counter.c", "cc");
    let component_config = output.join("c_counter.nros.toml");
    write_native_component_config(
        &component_config,
        "c_counter",
        "nros_component_counter",
        "c",
        &archive,
        "c_counter.metadata.json",
    );
    let source_metadata = output.join("c_counter.metadata.json");
    write_native_source_metadata(
        &source_metadata,
        "c_counter",
        "nros_component_counter",
        "c",
        "counter_node",
        "counter",
        "/c",
    );

    let cpp_archive = build_native_counter_archive(&output, "cpp_counter", "counter.cpp", "c++");
    let cpp_component_config = output.join("cpp_counter.nros.toml");
    write_native_component_config(
        &cpp_component_config,
        "cpp_counter",
        "nros_component_cpp_counter",
        "cpp",
        &cpp_archive,
        "cpp_counter.metadata.json",
    );
    let cpp_source_metadata = output.join("cpp_counter.metadata.json");
    write_native_source_metadata(
        &cpp_source_metadata,
        "cpp_counter",
        "nros_component_cpp_counter",
        "cpp",
        "cpp_counter_node",
        "cpp_counter",
        "/cpp",
    );

    let mut plan = fixture_plan("plan_multi_instance.json");
    retarget_plan_to_fixture_component(&mut plan);
    add_native_counter_component(
        &mut plan,
        "c_counter",
        "c_counter::counter",
        "nros_component_counter",
        "c",
        "counter",
        "/c",
        "counter_node",
        &component_config,
        &source_metadata,
    );
    add_native_counter_component(
        &mut plan,
        "cpp_counter",
        "cpp_counter::counter",
        "nros_component_cpp_counter",
        "cpp",
        "cpp_counter",
        "/cpp",
        "cpp_counter_node",
        &cpp_component_config,
        &cpp_source_metadata,
    );
    fs::write(
        &plan_path,
        serde_json::to_string_pretty(&plan).expect("serialize mixed C plan"),
    )
    .expect("write mixed C plan");

    check::run(check::Args {
        plan: plan_path.clone(),
    })
    .expect("check command validates generated mixed C plan");
    build::run(build::Args {
        project: Some(fixture),
        system_plan: Some(plan_path),
        system_output: Some(generated_dir.clone()),
        system_package: Some("nros-e2e-generated-mixed-c".to_string()),
        nano_ros_workspace: Some(nano_ros_workspace()),
        release: false,
        target: None,
        passthrough: Vec::new(),
        launch: None,
        system_pkg: None,
        metadata: Vec::new(),
        manifest: Vec::new(),
        launch_arg: Vec::new(),
        out_dir: None,
    })
    .expect("build command links generated package with C component archive");

    let build_rs = fs::read_to_string(generated_dir.join("build.rs")).expect("read build.rs");
    assert!(build_rs.contains("cargo:rustc-link-lib=static=c_counter"));
    assert!(build_rs.contains("cargo:rustc-link-lib=static=cpp_counter"));
    let binary = out_dir
        .join("target")
        .join(&plan.build.target)
        .join(if plan.build.profile == "release" {
            "release"
        } else {
            "debug"
        })
        .join("nros-e2e-generated-mixed-c");
    assert!(
        binary.is_file(),
        "generated mixed C binary exists at {}",
        binary.display()
    );
}

#[test]
fn fixture_workspace_builds_generated_service_action_package() {
    let fixture = fixture_workspace();
    let output = temp_output("orchestration_e2e_service_action");
    let out_dir = output.join("build/e2e_system/nros");
    let generated_dir = out_dir.join("generated-service-action");
    let plan_path = out_dir.join("nros-plan-service-action.json");
    fs::create_dir_all(&out_dir).expect("create service/action output dir");

    let mut plan = fixture_plan("plan_service_action.json");
    retarget_plan_to_fixture_component(&mut plan);
    fs::write(
        &plan_path,
        serde_json::to_string_pretty(&plan).expect("serialize service/action plan"),
    )
    .expect("write service/action plan");

    check::run(check::Args {
        plan: plan_path.clone(),
    })
    .expect("check command validates generated service/action plan");
    build::run(build::Args {
        project: Some(fixture),
        system_plan: Some(plan_path),
        system_output: Some(generated_dir.clone()),
        system_package: Some("nros-e2e-generated-service-action".to_string()),
        nano_ros_workspace: Some(nano_ros_workspace()),
        release: false,
        target: None,
        passthrough: Vec::new(),
        launch: None,
        system_pkg: None,
        metadata: Vec::new(),
        manifest: Vec::new(),
        launch_arg: Vec::new(),
        out_dir: None,
    })
    .expect("build command compiles generated service/action package");

    let generated_tables =
        fs::read_to_string(generated_dir.join("build.rs")).expect("read generated build.rs");
    assert!(generated_tables.contains("register_service_raw_sized_on::<1024, 1024>"));
    assert!(
        generated_tables.contains("register_action_server_raw_sized_on::<1024, 1024, 1024, 4>")
    );
    let binary = out_dir
        .join("target")
        .join(&plan.build.target)
        .join(if plan.build.profile == "release" {
            "release"
        } else {
            "debug"
        })
        .join("nros-e2e-generated-service-action");
    assert!(
        binary.is_file(),
        "generated service/action binary exists at {}",
        binary.display()
    );
}

#[test]
fn build_launch_one_shot_runs_metadata_plan_generate_and_cargo() {
    // Phase 126.M4 — `nros build --launch` runs metadata + plan +
    // generation + cargo in one invocation. Output_dir is explicit so
    // the fixture's tree stays clean across runs; the build-binary
    // pointer comes back from the returned plan.
    let fixture = fixture_workspace();
    let output = temp_output("orchestration_e2e_one_shot");
    let out_root = output.join("build/e2e_system/nros");
    let generated_dir = out_root.join("generated");
    let demo_pkg = fixture.join("src/demo_pkg");

    build::run(build::Args {
        project: Some(fixture.clone()),
        system_plan: None,
        system_output: Some(generated_dir.clone()),
        system_package: Some("nros-e2e-generated-one-shot".to_string()),
        nano_ros_workspace: Some(nano_ros_workspace()),
        release: false,
        target: None,
        passthrough: Vec::new(),
        launch: Some(demo_pkg.join("launch/system.launch.xml")),
        system_pkg: Some("e2e_system".to_string()),
        metadata: vec![fixture.join("artifacts/talker.metadata.json")],
        manifest: vec![demo_pkg.join("manifest/system.launch.yaml")],
        launch_arg: Vec::new(),
        out_dir: Some(out_root.clone()),
    })
    .expect("build --launch one-shot mode completes metadata+plan+generate+cargo");

    let plan_path = out_root.join("nros-plan.json");
    assert!(plan_path.is_file(), "plan written at {}", plan_path.display());
    let plan: NrosPlan = serde_json::from_str(&fs::read_to_string(&plan_path).expect("read plan"))
        .expect("plan parses");
    assert_eq!(plan.system, "e2e_system");
    assert!(generated_dir.join("Cargo.toml").is_file());
    assert!(generated_dir.join("src/main.rs").is_file());
    let binary = out_root
        .join("target")
        .join(&plan.build.target)
        .join("debug")
        .join("nros-e2e-generated-one-shot");
    assert!(binary.is_file(), "one-shot binary exists at {}", binary.display());
}

fn fixture_workspace() -> PathBuf {
    codegen_root().join("testing_workspaces/orchestration_e2e")
}

fn codegen_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("codegen root ancestor")
        .to_path_buf()
}

fn nano_ros_workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("nano-ros workspace ancestor")
        .to_path_buf()
}

fn fixture_plan(name: &str) -> NrosPlan {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/orchestration")
        .join(name);
    serde_json::from_str(&fs::read_to_string(&path).expect("read plan fixture"))
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn retarget_plan_to_fixture_component(plan: &mut NrosPlan) {
    for component in &mut plan.components {
        if component.id == "demo_nodes_rs::talker" {
            component.id = "demo_pkg::talker".to_string();
            component.package = "demo_pkg".to_string();
            component.component = "talker".to_string();
        }
    }
    for instance in &mut plan.instances {
        if instance.component == "demo_nodes_rs::talker" {
            instance.component = "demo_pkg::talker".to_string();
            instance.package = "demo_pkg".to_string();
        }
    }
}

fn retarget_plan_to_freertos(plan: &mut NrosPlan) {
    plan.build.target = "thumbv7m-none-eabi".to_string();
    plan.build.board = "freertos".to_string();
    plan.build.rmw = "zenoh".to_string();
    plan.build.profile = "release".to_string();
}

fn add_second_instance(plan: &mut NrosPlan) {
    let mut instance = plan.instances[0].clone();
    let old_instance_id = instance.id.clone();
    let new_instance_id = "talker_clone";
    instance.id = new_instance_id.to_string();
    instance.launch_name = "talker_clone".to_string();
    instance.namespace = "/clone".to_string();
    for node in &mut instance.nodes {
        let old_node_id = node.id.clone();
        node.id = node.id.replacen(&old_instance_id, new_instance_id, 1);
        node.resolved_name = "/clone/talker".to_string();
        node.namespace = "/clone".to_string();
        for entity in &mut node.entities {
            rewrite_entity_id(entity, &old_instance_id, new_instance_id);
        }
        for parameter in &mut instance.parameters {
            if parameter.node == old_node_id {
                parameter.node = node.id.clone();
            }
        }
    }
    for callback in &mut instance.callbacks {
        callback.id = callback.id.replacen(&old_instance_id, new_instance_id, 1);
    }
    for binding in &mut instance.sched_bindings {
        binding.callback = binding
            .callback
            .replacen(&old_instance_id, new_instance_id, 1);
    }
    for interface in &mut plan.interfaces {
        let extra = interface
            .used_by
            .iter()
            .filter(|entity| entity.starts_with(&old_instance_id))
            .map(|entity| entity.replacen(&old_instance_id, new_instance_id, 1))
            .collect::<Vec<_>>();
        interface.used_by.extend(extra);
        interface.used_by.sort();
        interface.used_by.dedup();
    }
    plan.instances.push(instance);
}

#[allow(clippy::too_many_arguments)]
fn add_native_counter_component(
    plan: &mut NrosPlan,
    package: &str,
    component_id: &str,
    symbol: &str,
    language: &str,
    executable: &str,
    namespace: &str,
    source_node: &str,
    config: &Path,
    metadata: &Path,
) {
    plan.components.push(PlanComponent {
        id: component_id.to_string(),
        package: package.to_string(),
        component: symbol.to_string(),
        language: language.to_string(),
        source_metadata: metadata.display().to_string(),
        component_config: Some(config.display().to_string()),
    });

    let mut instance = plan.instances[0].clone();
    instance.id = package.to_string();
    instance.component = component_id.to_string();
    instance.package = package.to_string();
    instance.executable = executable.to_string();
    instance.launch_name = executable.to_string();
    instance.namespace = namespace.to_string();
    instance.parameters.clear();
    instance.callbacks.clear();
    instance.sched_bindings.clear();
    instance.trace.source_metadata = metadata.display().to_string();
    instance.trace.launch_record_entity = package.to_string();
    instance.nodes.truncate(1);
    let node = &mut instance.nodes[0];
    node.id = format!("{package}/{source_node}");
    node.source_node = source_node.to_string();
    node.resolved_name = format!("{namespace}/{executable}");
    node.namespace = namespace.to_string();
    node.entities.clear();
    plan.instances.push(instance);
}

fn write_native_component_config(
    path: &Path,
    package: &str,
    symbol: &str,
    language: &str,
    archive: &Path,
    source_metadata: &str,
) {
    fs::write(
        path,
        format!(
            r#"version = 1
package = "{package}"
component = "{symbol}"
language = "{language}"

[linkage]
crate_name = ""
executable = ""
exported_symbol = "{symbol}"
static_library = "{}"

[metadata]
source_metadata = "{source_metadata}"

[overrides]
parameters = {{}}
remaps = []
"#,
            archive.display()
        ),
    )
    .expect("write native component config");
}

fn write_native_source_metadata(
    path: &Path,
    package: &str,
    symbol: &str,
    language: &str,
    node_id: &str,
    node_name: &str,
    namespace: &str,
) {
    fs::write(
        path,
        format!(
            r#"{{
  "version": 1,
  "package": "{package}",
  "component": "{symbol}",
  "language": "{language}",
  "executable": null,
  "exported_symbol": "{symbol}",
  "nodes": [
    {{
      "id": "{node_id}",
      "name": "{node_name}",
      "namespace": "{namespace}",
      "entities": [],
      "parameters": []
    }}
  ],
  "callbacks": []
}}"#
        ),
    )
    .expect("write native source metadata");
}

fn build_native_counter_archive(
    output: &Path,
    package: &str,
    source_file: &str,
    compiler: &str,
) -> PathBuf {
    let build_dir = output.join(format!("{package}_build"));
    fs::create_dir_all(&build_dir).expect("create native counter build dir");
    let object = build_dir.join("counter.o");
    let archive = build_dir.join(format!("lib{package}.a"));
    let source = fixture_workspace()
        .join("src")
        .join(package)
        .join(source_file);
    let cc_status = Command::new(compiler)
        .arg("-c")
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .status()
        .unwrap_or_else(|error| panic!("compile {package} fixture: {error}"));
    assert!(cc_status.success(), "compile {package} fixture failed");
    let ar_status = Command::new("ar")
        .arg("crs")
        .arg(&archive)
        .arg(&object)
        .status()
        .unwrap_or_else(|error| panic!("archive {package} fixture: {error}"));
    assert!(ar_status.success(), "archive {package} fixture failed");
    archive
}

fn rewrite_entity_id(entity: &mut PlanEntity, old_instance_id: &str, new_instance_id: &str) {
    match entity {
        PlanEntity::Publisher {
            id, resolved_name, ..
        }
        | PlanEntity::Subscriber {
            id, resolved_name, ..
        }
        | PlanEntity::ServiceServer {
            id, resolved_name, ..
        }
        | PlanEntity::ServiceClient {
            id, resolved_name, ..
        }
        | PlanEntity::ActionServer {
            id, resolved_name, ..
        }
        | PlanEntity::ActionClient {
            id, resolved_name, ..
        } => {
            *id = id.replacen(old_instance_id, new_instance_id, 1);
            *resolved_name = resolved_name.replacen("/talker", "/clone/talker", 1);
        }
        PlanEntity::Timer { id, .. } => {
            *id = id.replacen(old_instance_id, new_instance_id, 1);
        }
    }
}

fn assert_freertos_binary_boots(binary: &Path) {
    let output = Command::new("timeout")
        .arg("8s")
        .arg("qemu-system-arm")
        .args([
            "-cpu",
            "cortex-m3",
            "-machine",
            "mps2-an385",
            "-nographic",
            "-semihosting-config",
            "enable=on,target=native",
            "-kernel",
        ])
        .arg(binary)
        .output()
        .unwrap_or_else(|error| panic!("run qemu-system-arm for {}: {error}", binary.display()));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.code() == Some(124) || output.status.success(),
        "generated FreeRTOS binary exited unexpectedly with {:?}\n{}",
        output.status,
        combined
    );
    assert!(
        combined.contains("nros QEMU FreeRTOS Platform"),
        "generated FreeRTOS binary did not print platform banner\n{}",
        combined
    );
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_local_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral localhost port");
    listener
        .local_addr()
        .expect("ephemeral listener local address")
        .port()
}

fn start_zenohd(port: u16) -> ChildGuard {
    let zenohd = nano_ros_workspace().join("build/zenohd/zenohd");
    assert!(
        zenohd.is_file(),
        "zenohd binary missing at {}; run `just build-zenohd`",
        zenohd.display()
    );

    let child = Command::new(&zenohd)
        .arg("--listen")
        .arg(format!("tcp/127.0.0.1:{port}"))
        .arg("--no-multicast-scouting")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn zenohd {}: {error}", zenohd.display()));
    let guard = ChildGuard(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return guard;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("zenohd did not listen on tcp/127.0.0.1:{port}");
}

fn assert_generated_binary_spins(binary: &Path, port: u16) {
    let mut child = Command::new(binary)
        .env("NROS_LOCATOR", format!("tcp/127.0.0.1:{port}"))
        .env("NROS_SESSION_MODE", "client")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn generated binary {}: {error}", binary.display()));

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Some(status) = child
            .try_wait()
            .expect("poll generated binary process status")
        {
            let output = child
                .wait_with_output()
                .expect("collect generated binary output");
            panic!(
                "generated binary exited early with {status}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(50));
    }

    child.kill().expect("stop spinning generated binary");
    let status = child.wait().expect("wait for stopped generated binary");
    assert!(
        !status.success(),
        "generated binary should still be spinning until the test stops it"
    );
}

fn temp_output(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{name}-{}-{stamp}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}
