//! Generated orchestration package writer.
//!
//! This module deliberately treats `nros-plan.json` as an opaque input path.
//! Agent A owns the final plan schema; generated package `build.rs` is the
//! host-side adapter that will be tightened once that schema lands.

use eyre::{Context, Result};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use super::{
    ComponentConfig, NrosPlan,
    plan::{PlanBuildOptions, PlanEntity, PlanInstance, PlanSchedContext},
    schema::{DeadlinePolicy, ParameterValue, SchedClass},
};

const CARGO_TEMPLATE: &str = include_str!("../../templates/orchestration/Cargo.toml.jinja");
const BUILD_TEMPLATE: &str = include_str!("../../templates/orchestration/build.rs.jinja");
const MAIN_TEMPLATE: &str = include_str!("../../templates/orchestration/main.rs.jinja");

#[derive(Debug, Clone)]
pub struct GenerateOptions {
    pub package_name: String,
    pub output_dir: PathBuf,
    pub plan_path: PathBuf,
    pub nros_path: PathBuf,
    pub nros_orchestration_path: PathBuf,
    pub component_workspace: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct GeneratedPackage {
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub plan_path: PathBuf,
}

pub fn generate_package(options: &GenerateOptions) -> Result<GeneratedPackage> {
    let src_dir = options.output_dir.join("src");
    fs::create_dir_all(&src_dir).wrap_err_with(|| {
        format!(
            "failed to create generated package src dir {}",
            src_dir.display()
        )
    })?;

    let plan = load_plan(&options.plan_path)?;
    let cargo_toml = render_cargo_toml(options, &plan);
    let build_rs = render_build_rs(options, &plan);
    let cargo_config = render_cargo_config(&plan);

    write_if_changed(&options.output_dir.join("Cargo.toml"), &cargo_toml)?;
    write_if_changed(&options.output_dir.join("build.rs"), &build_rs)?;
    write_if_changed(&src_dir.join("main.rs"), MAIN_TEMPLATE)?;
    if let Some(cargo_config) = cargo_config {
        let cargo_dir = options.output_dir.join(".cargo");
        fs::create_dir_all(&cargo_dir).wrap_err_with(|| {
            format!(
                "failed to create generated package cargo config dir {}",
                cargo_dir.display()
            )
        })?;
        write_if_changed(&cargo_dir.join("config.toml"), &cargo_config)?;
    }

    Ok(GeneratedPackage {
        root: options.output_dir.clone(),
        manifest_path: options.output_dir.join("Cargo.toml"),
        plan_path: options.plan_path.clone(),
    })
}

fn render_cargo_toml(options: &GenerateOptions, plan: &NrosPlan) -> String {
    CARGO_TEMPLATE
        .replace("{{ package_name }}", &options.package_name)
        .replace(
            "{{ default_features }}",
            &toml_string_array(&generated_default_features(&plan.build)),
        )
        .replace("{{ nros_path }}", &path_for_template(&options.nros_path))
        .replace(
            "{{ nros_orchestration_path }}",
            &path_for_template(&options.nros_orchestration_path),
        )
        .replace(
            "{{ component_dependencies }}",
            &format!(
                "{}{}{}",
                render_platform_dependencies(options, plan),
                render_backend_dependencies(options, plan),
                render_component_dependencies(options, plan)
            ),
        )
}

fn render_build_rs(options: &GenerateOptions, plan: &NrosPlan) -> String {
    let generated_tables = render_generated_tables(&plan);
    BUILD_TEMPLATE
        .replace("{{ plan_path }}", &path_for_template(&options.plan_path))
        .replace(
            "{{ native_link_directives }}",
            &render_native_link_directives(options, plan),
        )
        .replace("{{ cfg_directives }}", &render_cfg_directives(plan))
        .replace(
            "{{ generated_tables_literal }}",
            &format!("{generated_tables:?}"),
        )
}

fn render_cfg_directives(plan: &NrosPlan) -> String {
    if plan.build.cfg.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (key, value) in &plan.build.cfg {
        let value_str = match value {
            ParameterValue::Bool(b) => b.to_string(),
            ParameterValue::Integer(n) => n.to_string(),
            ParameterValue::Float(f) => format!("{f:?}"),
            ParameterValue::String(s) => s.clone(),
            _ => continue,
        };
        let key = sanitize_cfg_key(key);
        out.push_str(&format!(
            "    println!(\"cargo::rustc-cfg={key}=\\\"{}\\\"\");\n",
            escape_cfg_value(&value_str),
        ));
        out.push_str(&format!(
            "    println!(\"cargo::rustc-check-cfg=cfg({key}, values(any()))\");\n",
        ));
    }
    out
}

fn sanitize_cfg_key(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn escape_cfg_value(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

#[derive(Debug, Clone)]
struct NativeComponentLink {
    component_id: String,
    library_path: PathBuf,
}

fn render_native_link_directives(options: &GenerateOptions, plan: &NrosPlan) -> String {
    native_component_links(options, plan)
        .into_iter()
        .map(|link| {
            let search_dir = link
                .library_path
                .parent()
                .map(path_for_template)
                .unwrap_or_default();
            let lib_name = static_library_name(&link.library_path)
                .unwrap_or_else(|| link.component_id.replace([':', '-'], "_"));
            format!(
                "    println!(\"cargo:rerun-if-changed={}\");\n    println!(\"cargo:rustc-link-search=native={search_dir}\");\n    println!(\"cargo:rustc-link-lib=static={lib_name}\");\n",
                path_for_template(&link.library_path),
            )
        })
        .collect()
}

fn render_cargo_config(plan: &NrosPlan) -> Option<String> {
    if platform_feature(&plan.build.board, &plan.build.target) != Some("platform-freertos") {
        return None;
    }
    Some(
        r#"[target.thumbv7m-none-eabi]
runner = "qemu-system-arm -cpu cortex-m3 -machine mps2-an385 -nographic -semihosting-config enable=on,target=native -kernel"
rustflags = [
    "-C", "link-arg=-Tmps2_an385.ld",
    "-C", "link-arg=--nmagic",
]
"#
        .to_string(),
    )
}

fn path_for_template(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn render_component_dependencies(options: &GenerateOptions, plan: &NrosPlan) -> String {
    let Some(workspace) = &options.component_workspace else {
        return String::new();
    };
    let mut deps = BTreeMap::new();
    for component in plan
        .components
        .iter()
        .filter(|component| matches!(component.language.as_str(), "rust" | "Rust"))
    {
        let crate_name = rust_crate_name(component.id.as_str()).unwrap_or(&component.package);
        let package_root = workspace.join("src").join(&component.package);
        if package_root.join("Cargo.toml").is_file() {
            deps.insert(crate_name.to_string(), package_root);
        }
    }
    deps.into_iter()
        .map(|(crate_name, path)| {
            format!(
                "{crate_name} = {{ path = \"{}\", default-features = false }}\n",
                path_for_template(&path)
            )
        })
        .collect()
}

fn native_component_links(options: &GenerateOptions, plan: &NrosPlan) -> Vec<NativeComponentLink> {
    plan.components
        .iter()
        .filter(|component| !matches!(component.language.as_str(), "rust" | "Rust"))
        .filter_map(|component| {
            let config_path = component.component_config.as_deref().and_then(|path| {
                resolve_workspace_path(options.component_workspace.as_deref(), path)
            });
            let library_path = config_path
                .as_deref()
                .and_then(|path| component_static_library(path).ok().flatten())?;
            Some(NativeComponentLink {
                component_id: component.id.clone(),
                library_path,
            })
        })
        .collect()
}

fn resolve_workspace_path(workspace: Option<&Path>, raw: &str) -> Option<PathBuf> {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return Some(path);
    }
    workspace.map(|workspace| workspace.join(path))
}

fn component_static_library(config_path: &Path) -> Result<Option<PathBuf>> {
    let raw = fs::read_to_string(config_path)
        .wrap_err_with(|| format!("failed to read {}", config_path.display()))?;
    let config: ComponentConfig = toml::from_str(&raw)
        .wrap_err_with(|| format!("failed to parse {}", config_path.display()))?;
    Ok(config.linkage.static_library.map(|raw| {
        let path = PathBuf::from(raw);
        if path.is_absolute() {
            path
        } else {
            config_path
                .parent()
                .map(|parent| parent.join(&path))
                .unwrap_or(path)
        }
    }))
}

fn static_library_name(path: &Path) -> Option<String> {
    let stem = path.file_name()?.to_str()?;
    let stem = stem.strip_suffix(".a").unwrap_or(stem);
    Some(stem.strip_prefix("lib").unwrap_or(stem).to_string())
}

fn render_platform_dependencies(options: &GenerateOptions, plan: &NrosPlan) -> String {
    let Some(workspace) = workspace_from_nros_path(&options.nros_path) else {
        return String::new();
    };
    match platform_feature(&plan.build.board, &plan.build.target) {
        Some("platform-posix") => format!(
            "nros-platform-cffi = {{ path = \"{}\", default-features = false, features = [\"posix-c-port\"] }}\n",
            path_for_template(&workspace.join("packages/core/nros-platform-cffi")),
        ),
        Some("platform-freertos") => format!(
            "nros-board-mps2-an385-freertos = {{ path = \"{}\" }}\npanic-semihosting = {{ version = \"0.6\", features = [\"exit\"] }}\n",
            path_for_template(&workspace.join("packages/boards/nros-board-mps2-an385-freertos")),
        ),
        _ => String::new(),
    }
}

fn render_backend_dependencies(options: &GenerateOptions, plan: &NrosPlan) -> String {
    let Some(workspace) = workspace_from_nros_path(&options.nros_path) else {
        return String::new();
    };
    match plan.build.rmw.as_str() {
        "zenoh" | "rmw-zenoh" | "rmw-zenoh-cffi" => format!(
            "nros-rmw-zenoh = {{ path = \"{}\", default-features = false, features = {} }}\n",
            path_for_template(&workspace.join("packages/zpico/nros-rmw-zenoh")),
            toml_string_array(&backend_features(&plan.build, "zenoh")),
        ),
        "xrce" | "rmw-xrce" | "rmw-xrce-cffi" => format!(
            "nros-rmw-xrce-cffi = {{ path = \"{}\", default-features = false, features = {} }}\n",
            path_for_template(&workspace.join("packages/xrce/nros-rmw-xrce-cffi")),
            toml_string_array(&backend_features(&plan.build, "xrce")),
        ),
        "dds" | "rmw-dds" | "rmw-dds-cffi" => format!(
            "nros-rmw-dds = {{ path = \"{}\", default-features = false, features = {} }}\n",
            path_for_template(&workspace.join("packages/dds/nros-rmw-dds")),
            toml_string_array(&backend_features(&plan.build, "dds")),
        ),
        _ => String::new(),
    }
}

fn workspace_from_nros_path(nros_path: &Path) -> Option<PathBuf> {
    nros_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

fn backend_features(build: &PlanBuildOptions, _backend: &str) -> Vec<String> {
    let mut features = Vec::new();
    if uses_std(build) {
        features.push("std".to_string());
    }
    if let Some(platform) = platform_feature(&build.board, &build.target) {
        features.push(platform.to_string());
    }
    // Phase 128.E.1: `link-tcp` / `link-udp-unicast` deleted (no-op aliases).
    // Zenoh ships TCP/UDP always-on at the zpico-sys layer.
    features
}

fn write_if_changed(path: &Path, contents: &str) -> Result<()> {
    if fs::read_to_string(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    fs::write(path, contents).wrap_err_with(|| format!("failed to write {}", path.display()))
}

fn load_plan(path: &Path) -> Result<NrosPlan> {
    let raw =
        fs::read_to_string(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).wrap_err_with(|| format!("failed to parse {}", path.display()))
}

fn generated_default_features(build: &PlanBuildOptions) -> Vec<String> {
    let mut features = Vec::new();
    if uses_std(build) {
        features.push("std".to_string());
    }
    if let Some(platform) = platform_feature(&build.board, &build.target) {
        features.push(format!("nros/{platform}"));
        if platform == "platform-freertos" {
            features.push(platform.to_string());
        }
    }
    if uses_rmw_cffi(&build.rmw) {
        features.push("nros/rmw-cffi".to_string());
        features.push("nros-orchestration/rmw-cffi".to_string());
        // Phase 128.C.3: per-backend `nros/rmw-<name>-cffi` features were
        // deleted. Backend selection now flows from depending on the
        // concrete `nros-rmw-<name>` crate (added via
        // `render_backend_dependencies`) — the linkme RMW_INIT_ENTRIES
        // walker discovers it on first `Executor::open`.
    }
    for feature in build
        .features
        .iter()
        .filter_map(|feature| generated_feature(feature))
    {
        features.push(feature);
    }
    dedup(features)
}

fn uses_std(build: &PlanBuildOptions) -> bool {
    matches!(build.board.as_str(), "native" | "posix")
        || build.target.contains("linux")
        || build.target.contains("darwin")
        || build.target.contains("apple")
        || build.target.contains("windows")
        || build.target.contains("freebsd")
}

fn platform_feature(board: &str, target: &str) -> Option<&'static str> {
    match board {
        "native" | "posix" => Some("platform-posix"),
        "zephyr" => Some("platform-zephyr"),
        "freertos" | "freeRTOS" | "FreeRTOS" => Some("platform-freertos"),
        "nuttx" | "NuttX" => Some("platform-nuttx"),
        "threadx" | "ThreadX" => Some("platform-threadx"),
        "baremetal" | "bare-metal" => Some("platform-bare-metal"),
        "orin-spe" => Some("platform-orin-spe"),
        _ if target.contains("linux") => Some("platform-posix"),
        _ => None,
    }
}

fn generated_feature(feature: &str) -> Option<String> {
    match feature {
        "std" => Some("std".to_string()),
        "rmw-cffi" => Some("nros/rmw-cffi".to_string()),
        // Backend choice is encoded via the concrete `nros-rmw-<name>`
        // path-dep, not a feature on the nros crate (Phase 128.C.3).
        "rmw-zenoh" | "rmw-zenoh-cffi" | "rmw-xrce" | "rmw-xrce-cffi" | "rmw-dds"
        | "rmw-dds-cffi" => None,
        feature if feature.starts_with("nros/") || feature.starts_with("nros-orchestration/") => {
            Some(feature.to_string())
        }
        _ => None,
    }
}

fn uses_rmw_cffi(rmw: &str) -> bool {
    !matches!(rmw, "" | "none")
}

fn dedup(features: Vec<String>) -> Vec<String> {
    features
        .into_iter()
        .fold(Vec::new(), |mut deduped, feature| {
            if !deduped.contains(&feature) {
                deduped.push(feature);
            }
            deduped
        })
}

fn toml_string_array(values: &[String]) -> String {
    let entries = values
        .iter()
        .map(|value| format!("{:?}", value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{entries}]")
}

fn render_generated_tables(plan: &NrosPlan) -> String {
    let schema = format!("nano-ros/plan/v{}", plan.version);
    let callback_count = plan
        .instances
        .iter()
        .map(|instance| instance.callbacks.len())
        .sum::<usize>();
    let max_nodes = plan
        .instances
        .iter()
        .map(|instance| instance.nodes.len())
        .sum::<usize>();
    let max_sched_contexts = plan.sched_contexts.len() + 1;
    let max_parameters = plan
        .instances
        .iter()
        .map(|instance| instance.parameters.len())
        .sum::<usize>();
    let max_interfaces = plan.interfaces.len();

    let mut out = String::new();
    out.push_str("#[allow(unused_imports)]\n");
    out.push_str("use nros_orchestration::{CallbackBindingSpec, CapacitySpec, ComponentLanguage, NodeSpec, PlanId, SchedClassSpec, SchedContextSpec, SystemSpec};\n");
    out.push_str("#[allow(unused_imports)]\n");
    out.push_str("use nros_orchestration::{CallbackHandleTable, ComponentSpec, InstanceSpec, ParameterSpec, ParameterValue};\n");
    out.push_str("#[allow(unused_imports)]\n");
    out.push_str("use nros_orchestration::{DeadlinePolicySpec, PrioritySpec};\n\n");
    out.push_str(&format!(
        "pub const CALLBACK_COUNT: usize = {callback_count};\n"
    ));
    out.push_str(&format!(
        "pub const SCHED_CONTEXT_COUNT: usize = {};\n\n",
        plan.sched_contexts.len()
    ));
    render_backend_register_fn(&mut out, plan);
    render_native_component_ffi(&mut out, plan);
    render_components(&mut out, plan);
    render_instances(&mut out, plan);
    render_nodes(&mut out, plan);
    render_parameters(&mut out, plan);
    out.push_str(&format!(
        "pub static SCHED_CONTEXTS: [SchedContextSpec; {}] = [\n",
        plan.sched_contexts.len()
    ));
    for sc in &plan.sched_contexts {
        out.push_str(&render_sched_context(sc));
    }
    out.push_str("];\n\n");
    let bindings = collect_callback_bindings(plan);
    out.push_str(&format!(
        "pub static CALLBACK_BINDINGS: [CallbackBindingSpec; {}] = [\n",
        bindings.len()
    ));
    for (callback_index, sched_context_index) in bindings {
        out.push_str(&format!(
            "    CallbackBindingSpec {{ callback_index: {callback_index}, sched_context_index: {sched_context_index} }},\n"
        ));
    }
    out.push_str("];\n\n");
    out.push_str(&format!(
        "pub static SYSTEM: SystemSpec = SystemSpec {{ schema: {schema:?}, plan_id: PlanId({plan_id}), capacities: CapacitySpec {{ max_nodes: {max_nodes}, max_callbacks: {callback_count}, max_sched_contexts: {max_sched_contexts}, max_parameters: {max_parameters}, max_interfaces: {max_interfaces} }}, components: &COMPONENTS, instances: &INSTANCES, nodes: &NODES, parameters: &PARAMETERS, sched_contexts: &SCHED_CONTEXTS, callback_bindings: &CALLBACK_BINDINGS }};\n\n",
        plan_id = stable_plan_id(plan),
    ));
    out.push_str("struct GeneratedNodeRuntime<'a> {\n");
    out.push_str("    executor: &'a mut nros::Executor,\n");
    out.push_str("    instance: &'static InstanceSpec,\n");
    out.push_str("}\n\n");
    out.push_str("impl nros::ComponentNodeRuntime for GeneratedNodeRuntime<'_> {\n");
    out.push_str(
        "    type NodeHandle = <nros::Executor as nros::ComponentNodeRuntime>::NodeHandle;\n\n",
    );
    out.push_str("    fn build_component_node(&mut self, id: nros::NodeId<'_>, options: nros::NodeOptions<'_>) -> nros::ComponentResult<Self::NodeHandle> {\n");
    out.push_str("        let planned = NODES.iter().find(|node| node.instance_id == self.instance.id && node.source_node == id.as_str());\n");
    out.push_str(
        "        let name = planned.map(|node| node.node_name).unwrap_or(options.name);\n",
    );
    out.push_str("        let namespace = planned.map(|node| node.namespace).unwrap_or(options.namespace);\n");
    out.push_str("        let domain_id = planned.and_then(|node| node.domain_id).unwrap_or(options.domain_id);\n");
    out.push_str("        self.executor.node_builder(name).namespace(namespace).domain_id(domain_id).build().map_err(|_| nros::ComponentError::Runtime)\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    out.push_str("#[allow(dead_code)]\nunsafe extern \"C\" fn noop_raw_subscription(_data: *const u8, _len: usize, _context: *mut core::ffi::c_void) {}\n");
    out.push_str("#[allow(dead_code)]\nunsafe extern \"C\" fn noop_raw_service(_req: *const u8, _req_len: usize, _resp: *mut u8, _resp_cap: usize, resp_len: *mut usize, _context: *mut core::ffi::c_void) -> bool {\n");
    out.push_str("    if !resp_len.is_null() { unsafe { *resp_len = 0; } }\n");
    out.push_str("    true\n");
    out.push_str("}\n");
    out.push_str("#[allow(dead_code)]\nunsafe extern \"C\" fn noop_raw_goal(_goal_id: *const nros::GoalId, _goal_data: *const u8, _goal_len: usize, _context: *mut core::ffi::c_void) -> nros::GoalResponse { nros::GoalResponse::AcceptAndDefer }\n");
    out.push_str("#[allow(dead_code)]\nunsafe extern \"C\" fn noop_raw_cancel(_goal_id: *const nros::GoalId, _status: nros::GoalStatus, _context: *mut core::ffi::c_void) -> nros::CancelResponse { nros::CancelResponse::Rejected }\n");
    out.push_str("#[allow(dead_code)]\nunsafe extern \"C\" fn noop_raw_accepted(_goal_id: *const nros::GoalId, _context: *mut core::ffi::c_void) {}\n\n");
    out.push_str("pub fn instantiate_components(executor: &mut nros::Executor, handles: &mut CallbackHandleTable<CALLBACK_COUNT>) -> Result<(), nros::NodeError> {\n");
    out.push_str("    for instance in INSTANCES.iter() {\n");
    out.push_str("        let mut node_runtime = GeneratedNodeRuntime { executor, instance };\n");
    out.push_str("        let mut runtime = nros::ComponentRuntimeAdapter::<_, MAX_NODES, MAX_ENTITIES, CALLBACK_COUNT>::new(&mut node_runtime);\n");
    out.push_str("        match instance.component_id {\n");
    for component in &plan.components {
        if matches!(component.language.as_str(), "rust" | "Rust") {
            if let Some(path) = rust_component_type_path(&component.id) {
                out.push_str(&format!(
                    "            {id:?} => nros::register_component::<{path}>(&mut runtime).map_err(|_| nros::NodeError::NotInitialized)?,\n",
                    id = component.id,
                ));
            }
        } else {
            let fn_name = native_register_fn_name(&component.id);
            out.push_str(&format!(
                "            {id:?} => unsafe {{ {fn_name}(&mut node_runtime) }}?,\n",
                id = component.id,
            ));
        }
    }
    out.push_str("            _ => return Err(nros::NodeError::NotInitialized),\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("    instantiate_callback_handles(executor, handles)?;\n");
    out.push_str("    Ok(())\n");
    out.push_str("}\n");
    out.push_str("\nfn instantiate_callback_handles(executor: &mut nros::Executor, handles: &mut CallbackHandleTable<CALLBACK_COUNT>) -> Result<(), nros::NodeError> {\n");
    for line in render_callback_registrations(plan) {
        out.push_str(&line);
    }
    out.push_str("    Ok(())\n");
    out.push_str("}\n");
    out
}

fn render_native_component_ffi(out: &mut String, plan: &NrosPlan) {
    let native_components = plan
        .components
        .iter()
        .filter(|component| !matches!(component.language.as_str(), "rust" | "Rust"))
        .collect::<Vec<_>>();
    if native_components.is_empty() {
        return;
    }

    out.push_str("use core::ffi::{c_char, c_void, CStr};\n\n");
    out.push_str("#[repr(C)]\nstruct NrosCComponentNodeOptions { name: *const c_char, namespace_: *const c_char, domain_id: u32 }\n");
    out.push_str("#[repr(C)]\nstruct NrosCComponentNode { stable_id: *const c_char, runtime_handle: *mut c_void, context: *mut NrosCComponentContext }\n");
    out.push_str("#[repr(C)]\nstruct NrosCComponentEntityDescriptor { stable_id: *const c_char, node_id: *const c_char, kind: i32, source_name: *const c_char, type_name: *const c_char, type_hash: *const c_char, callback_id: *const c_char }\n");
    out.push_str("#[repr(C)]\nstruct NrosCComponentContextOps { create_node: Option<unsafe extern \"C\" fn(*mut c_void, *const c_char, *const NrosCComponentNodeOptions, *mut NrosCComponentNode) -> i32>, create_entity: Option<unsafe extern \"C\" fn(*mut c_void, *const NrosCComponentEntityDescriptor) -> i32>, record_callback_effect: Option<unsafe extern \"C\" fn(*mut c_void, *const c_char, i32, *const c_char) -> i32> }\n");
    out.push_str("#[repr(C)]\nstruct NrosCComponentContext { user_data: *mut c_void, ops: *const NrosCComponentContextOps }\n\n");
    out.push_str("const NROS_RET_OK: i32 = 0;\nconst NROS_RET_INVALID_ARGUMENT: i32 = -3;\n\n");
    out.push_str("static NROS_C_COMPONENT_OPS: NrosCComponentContextOps = NrosCComponentContextOps { create_node: Some(nros_c_component_create_node), create_entity: Some(nros_c_component_create_entity), record_callback_effect: Some(nros_c_component_record_callback_effect) };\n\n");
    out.push_str("unsafe extern \"C\" fn nros_c_component_create_node(user_data: *mut c_void, stable_id: *const c_char, options: *const NrosCComponentNodeOptions, out_node: *mut NrosCComponentNode) -> i32 {\n");
    out.push_str("    if user_data.is_null() || stable_id.is_null() || options.is_null() || out_node.is_null() { return NROS_RET_INVALID_ARGUMENT; }\n");
    out.push_str(
        "    let runtime = unsafe { &mut *(user_data as *mut GeneratedNodeRuntime<'_>) };\n",
    );
    out.push_str("    let stable_id = match unsafe { c_str_to_str(stable_id) } { Some(value) => value, None => return NROS_RET_INVALID_ARGUMENT };\n");
    out.push_str("    let options = unsafe { &*options };\n");
    out.push_str("    if options.name.is_null() || options.namespace_.is_null() { return NROS_RET_INVALID_ARGUMENT; }\n");
    out.push_str("    let name = match unsafe { c_str_to_str(options.name) } { Some(value) => value, None => return NROS_RET_INVALID_ARGUMENT };\n");
    out.push_str("    let namespace = match unsafe { c_str_to_str(options.namespace_) } { Some(value) => value, None => return NROS_RET_INVALID_ARGUMENT };\n");
    out.push_str("    let options = nros::NodeOptions::new(name).namespace(namespace).domain_id(options.domain_id);\n");
    out.push_str("    match nros::ComponentNodeRuntime::build_component_node(runtime, nros::NodeId(stable_id), options) { Ok(_) => { unsafe { (*out_node).stable_id = core::ptr::null(); (*out_node).runtime_handle = core::ptr::null_mut(); (*out_node).context = core::ptr::null_mut(); } NROS_RET_OK }, Err(_) => NROS_RET_INVALID_ARGUMENT }\n");
    out.push_str("}\n\n");
    out.push_str("unsafe extern \"C\" fn nros_c_component_create_entity(_user_data: *mut c_void, _descriptor: *const NrosCComponentEntityDescriptor) -> i32 { NROS_RET_OK }\n");
    out.push_str("unsafe extern \"C\" fn nros_c_component_record_callback_effect(_user_data: *mut c_void, _callback_id: *const c_char, _kind: i32, _entity_id: *const c_char) -> i32 { NROS_RET_OK }\n\n");
    out.push_str("unsafe fn c_str_to_str<'a>(ptr: *const c_char) -> Option<&'a str> { unsafe { CStr::from_ptr(ptr) }.to_str().ok() }\n\n");
    out.push_str("unsafe extern \"C\" {\n");
    for component in &native_components {
        out.push_str(&format!(
            "    #[link_name = {symbol:?}]\n    fn {fn_name}(context: *mut NrosCComponentContext) -> i32;\n",
            symbol = component.component,
            fn_name = native_symbol_fn_name(&component.id),
        ));
    }
    out.push_str("}\n\n");
    for component in &native_components {
        out.push_str(&format!(
            "unsafe fn {fn_name}(runtime: &mut GeneratedNodeRuntime<'_>) -> Result<(), nros::NodeError> {{\n    let mut context = NrosCComponentContext {{ user_data: runtime as *mut _ as *mut c_void, ops: &NROS_C_COMPONENT_OPS }};\n    let status = unsafe {{ {symbol_fn}(&mut context) }};\n    if status == NROS_RET_OK {{ Ok(()) }} else {{ Err(nros::NodeError::NotInitialized) }}\n}}\n\n",
            fn_name = native_register_fn_name(&component.id),
            symbol_fn = native_symbol_fn_name(&component.id),
        ));
    }
}

fn render_backend_register_fn(out: &mut String, plan: &NrosPlan) {
    out.push_str("pub fn register_backends() {\n");
    match plan.build.rmw.as_str() {
        "zenoh" | "rmw-zenoh" | "rmw-zenoh-cffi" => {
            out.push_str("    let _ = nros_rmw_zenoh::register();\n");
        }
        "xrce" | "rmw-xrce" | "rmw-xrce-cffi" => {
            out.push_str("    let _ = nros_rmw_xrce_cffi::register();\n");
        }
        "dds" | "rmw-dds" | "rmw-dds-cffi" => {
            out.push_str("    let _ = nros_rmw_dds::register();\n");
        }
        _ => {}
    }
    out.push_str("}\n\n");
}

fn render_components(out: &mut String, plan: &NrosPlan) {
    out.push_str(&format!(
        "pub static COMPONENTS: [ComponentSpec; {}] = [\n",
        plan.components.len()
    ));
    for component in &plan.components {
        out.push_str(&format!(
            "    ComponentSpec {{ id: {id:?}, package: {package:?}, symbol: {symbol:?}, language: ComponentLanguage::{language} }},\n",
            id = component.id,
            package = component.package,
            symbol = component.component,
            language = component_language(&component.language),
        ));
    }
    out.push_str("];\n\n");
}

fn render_instances(out: &mut String, plan: &NrosPlan) {
    out.push_str(&format!(
        "pub static INSTANCES: [InstanceSpec; {}] = [\n",
        plan.instances.len()
    ));
    let mut parameter_start = 0usize;
    for instance in &plan.instances {
        let parameter_len = instance.parameters.len();
        let node_name = instance
            .nodes
            .first()
            .map(|node| node.resolved_name.as_str())
            .unwrap_or(instance.launch_name.as_str());
        out.push_str(&format!(
            "    InstanceSpec {{ id: {id:?}, component_id: {component:?}, node_name: {node_name:?}, namespace: {namespace:?}, domain_id: None, parameter_start: {parameter_start}, parameter_len: {parameter_len} }},\n",
            id = instance.id,
            component = instance.component,
            namespace = instance.namespace,
        ));
        parameter_start += parameter_len;
    }
    out.push_str("];\n\n");
}

fn render_nodes(out: &mut String, plan: &NrosPlan) {
    let node_count = plan
        .instances
        .iter()
        .map(|instance| instance.nodes.len())
        .sum::<usize>();
    out.push_str(&format!("pub const MAX_NODES: usize = {node_count};\n"));
    let max_entities = plan
        .instances
        .iter()
        .map(|instance| {
            instance
                .nodes
                .iter()
                .map(|node| node.entities.len())
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    out.push_str(&format!(
        "pub const MAX_ENTITIES: usize = {max_entities};\n"
    ));
    out.push_str(&format!("pub static NODES: [NodeSpec; {node_count}] = [\n"));
    for instance in &plan.instances {
        for node in &instance.nodes {
            let node_name = final_node_name(&node.resolved_name, &node.namespace);
            out.push_str(&format!(
                "    NodeSpec {{ instance_id: {instance_id:?}, node_id: {node_id:?}, source_node: {source_node:?}, node_name: {node_name:?}, namespace: {namespace:?}, domain_id: None }},\n",
                instance_id = instance.id,
                node_id = node.id,
                source_node = node.source_node,
                namespace = node.namespace,
            ));
        }
    }
    out.push_str("];\n\n");
}

fn render_parameters(out: &mut String, plan: &NrosPlan) {
    let rendered_parameters = plan
        .instances
        .iter()
        .flat_map(|instance| {
            instance.parameters.iter().filter_map(move |parameter| {
                render_parameter_value(&parameter.value).map(|value| {
                    format!(
                        "    ParameterSpec {{ instance_id: {instance_id:?}, name: {name:?}, value: {value} }},\n",
                        instance_id = instance.id,
                        name = parameter.name,
                    )
                })
            })
        })
        .collect::<Vec<_>>();
    out.push_str(&format!(
        "pub static PARAMETERS: [ParameterSpec; {}] = [\n",
        rendered_parameters.len()
    ));
    for parameter in rendered_parameters {
        out.push_str(&parameter);
    }
    out.push_str("];\n\n");
}

fn render_callback_registrations(plan: &NrosPlan) -> Vec<String> {
    let mut out = Vec::new();
    let mut callback_index = 0usize;
    for instance in &plan.instances {
        for callback in &instance.callbacks {
            match find_callback_entity(
                instance,
                callback.id.as_str(),
                callback.source_callback.as_str(),
            ) {
                Some((_node_id, PlanEntity::Timer { period_ms, .. })) => {
                    out.push(format!(
                        "    let handle_{callback_index} = executor.register_timer(nros::TimerDuration::from_millis({period_ms}), || {{}})?;\n"
                    ));
                    out.push(format!(
                        "    handles.set({callback_index}, handle_{callback_index}).map_err(|_| nros::NodeError::InvalidSchedContextBinding)?;\n"
                    ));
                }
                Some((
                    node_id,
                    PlanEntity::Subscriber {
                        resolved_name,
                        interface,
                        ..
                    },
                )) => {
                    out.push(format!(
                        "    let node_{callback_index} = NODES.iter().find(|node| node.node_id == {node_id:?}).ok_or(nros::NodeError::InvalidSchedContextBinding)?;\n"
                    ));
                    out.push(format!(
                        "    let node_handle_{callback_index} = executor.node_id_by_name(node_{callback_index}.node_name, node_{callback_index}.namespace).ok_or(nros::NodeError::InvalidSchedContextBinding)?;\n"
                    ));
                    out.push(format!(
                        "    let handle_{callback_index} = executor.register_subscription_raw_with_qos_sized_on::<1024>(node_handle_{callback_index}, {topic:?}, {type_name:?}, {type_hash:?}, nros::QosSettings::default().keep_last(1), noop_raw_subscription, core::ptr::null_mut())?;\n",
                        topic = resolved_name,
                        type_name = interface_type_name(interface),
                        type_hash = interface_type_hash(interface),
                    ));
                    out.push(format!(
                        "    handles.set({callback_index}, handle_{callback_index}).map_err(|_| nros::NodeError::InvalidSchedContextBinding)?;\n"
                    ));
                }
                Some((
                    node_id,
                    PlanEntity::ServiceServer {
                        resolved_name,
                        interface,
                        ..
                    },
                )) => {
                    out.push(format!(
                        "    let node_{callback_index} = NODES.iter().find(|node| node.node_id == {node_id:?}).ok_or(nros::NodeError::InvalidSchedContextBinding)?;\n"
                    ));
                    out.push(format!(
                        "    let node_handle_{callback_index} = executor.node_id_by_name(node_{callback_index}.node_name, node_{callback_index}.namespace).ok_or(nros::NodeError::InvalidSchedContextBinding)?;\n"
                    ));
                    out.push(format!(
                        "    let handle_{callback_index} = executor.register_service_raw_sized_on::<1024, 1024>(node_handle_{callback_index}, {service:?}, {type_name:?}, {type_hash:?}, noop_raw_service, core::ptr::null_mut())?;\n",
                        service = resolved_name,
                        type_name = interface_type_name(interface),
                        type_hash = interface_type_hash(interface),
                    ));
                    out.push(format!(
                        "    handles.set({callback_index}, handle_{callback_index}).map_err(|_| nros::NodeError::InvalidSchedContextBinding)?;\n"
                    ));
                }
                Some((
                    node_id,
                    PlanEntity::ActionServer {
                        resolved_name,
                        interface,
                        ..
                    },
                )) => {
                    out.push(format!(
                        "    let node_{callback_index} = NODES.iter().find(|node| node.node_id == {node_id:?}).ok_or(nros::NodeError::InvalidSchedContextBinding)?;\n"
                    ));
                    out.push(format!(
                        "    let node_handle_{callback_index} = executor.node_id_by_name(node_{callback_index}.node_name, node_{callback_index}.namespace).ok_or(nros::NodeError::InvalidSchedContextBinding)?;\n"
                    ));
                    out.push(format!(
                        "    let action_{callback_index} = executor.register_action_server_raw_sized_on::<1024, 1024, 1024, 4>(node_handle_{callback_index}, {action:?}, {type_name:?}, {type_hash:?}, noop_raw_goal, noop_raw_cancel, Some(noop_raw_accepted), core::ptr::null_mut())?;\n",
                        action = resolved_name,
                        type_name = interface_type_name(interface),
                        type_hash = interface_type_hash(interface),
                    ));
                    out.push(format!(
                        "    handles.set({callback_index}, action_{callback_index}.handle_id()).map_err(|_| nros::NodeError::InvalidSchedContextBinding)?;\n"
                    ));
                }
                _ => {
                    out.push(format!(
                        "    return Err(nros::NodeError::NotInitialized); // unsupported generated callback: {:?}\n",
                        callback.id
                    ));
                }
            }
            callback_index += 1;
        }
    }
    out
}

fn find_callback_entity<'a>(
    instance: &'a PlanInstance,
    callback_id: &str,
    source_callback: &str,
) -> Option<(&'a str, &'a PlanEntity)> {
    let mut callback_entities = Vec::new();
    for node in &instance.nodes {
        for entity in &node.entities {
            if entity_callback_id(entity).is_some_and(|entity_callback| {
                entity_callback == callback_id || entity_callback == source_callback
            }) {
                return Some((node.id.as_str(), entity));
            }
            if entity_callback_id(entity).is_some() {
                callback_entities.push((node.id.as_str(), entity));
            }
        }
    }
    if let Some(entity) = callback_entities.iter().copied().find(|(_, entity)| {
        matches!(entity, PlanEntity::Timer { .. }) && source_callback.contains("timer")
    }) {
        return Some(entity);
    }
    if let Some(entity) = callback_entities.iter().copied().find(|(_, entity)| {
        matches!(entity, PlanEntity::Subscriber { .. })
            && (source_callback.contains("message") || source_callback.contains("sub"))
    }) {
        return Some(entity);
    }
    if let Some(entity) = callback_entities
        .iter()
        .copied()
        .find(|(_, entity)| entity_matches_callback_text(entity, source_callback))
    {
        return Some(entity);
    }
    if callback_entities.len() == 1 {
        return callback_entities.first().copied();
    }
    None
}

fn entity_matches_callback_text(entity: &PlanEntity, source_callback: &str) -> bool {
    let text = match entity {
        PlanEntity::Publisher {
            id,
            source_entity,
            resolved_name,
            ..
        }
        | PlanEntity::Subscriber {
            id,
            source_entity,
            resolved_name,
            ..
        }
        | PlanEntity::ServiceServer {
            id,
            source_entity,
            resolved_name,
            ..
        }
        | PlanEntity::ServiceClient {
            id,
            source_entity,
            resolved_name,
            ..
        }
        | PlanEntity::ActionServer {
            id,
            source_entity,
            resolved_name,
            ..
        }
        | PlanEntity::ActionClient {
            id,
            source_entity,
            resolved_name,
            ..
        } => format!("{id} {source_entity} {resolved_name}"),
        PlanEntity::Timer {
            id, source_entity, ..
        } => format!("{id} {source_entity}"),
    };
    source_callback
        .trim_start_matches("cb_")
        .split('_')
        .filter(|token| token.len() > 2)
        .any(|token| text.contains(token))
}

fn entity_callback_id(entity: &PlanEntity) -> Option<&str> {
    match entity {
        PlanEntity::Subscriber { id, callback, .. } => callback.as_deref().or(Some(id.as_str())),
        PlanEntity::Timer { id, callback, .. } => callback.as_deref().or(Some(id.as_str())),
        PlanEntity::ServiceServer { id, callback, .. } => callback.as_deref().or(Some(id.as_str())),
        PlanEntity::ActionServer { id, callback, .. } => callback.as_deref().or(Some(id.as_str())),
        _ => None,
    }
}

fn interface_type_name(interface: &super::schema::InterfaceRef) -> String {
    let (namespace, name) = split_interface_name(&interface.name);
    format!("{}::{}::dds_::{}_", interface.package, namespace, name)
}

fn interface_type_hash(interface: &super::schema::InterfaceRef) -> String {
    format!("{}/{}", interface.package, interface.name)
}

fn split_interface_name(name: &str) -> (&str, &str) {
    name.split_once('/').unwrap_or(("msg", name))
}

fn render_sched_context(sc: &PlanSchedContext) -> String {
    format!(
        "    SchedContextSpec {{ id: {id:?}, class: SchedClassSpec::{class}, priority: PrioritySpec::{priority}, period_us: {period}, budget_us: {budget}, deadline_us: {deadline}, deadline_policy: DeadlinePolicySpec::{deadline_policy}, os_pri: {os_pri}, tt_window_offset_us: {tt_offset}, tt_window_duration_us: {tt_duration} }},\n",
        id = sc.id,
        class = sched_class(&sc.class),
        priority = priority(sc.priority),
        period = option_ms_to_us(sc.period_ms),
        budget = option_ms_to_us(sc.budget_ms),
        deadline = option_ms_to_us(sc.deadline_ms),
        deadline_policy = deadline_policy(&sc.deadline_policy),
        os_pri = sc.priority.unwrap_or(0),
        tt_offset = "None",
        tt_duration = option_ms_to_us(match sc.class {
            SchedClass::TimeTriggered => sc.period_ms,
            _ => None,
        }),
    )
}

fn collect_callback_bindings(plan: &NrosPlan) -> Vec<(usize, usize)> {
    let mut bindings = Vec::new();
    let mut callback_index = 0usize;
    for instance in &plan.instances {
        for callback in &instance.callbacks {
            let sched_context_index = plan
                .sched_contexts
                .iter()
                .position(|context| context.id == callback.sched_context)
                .map(|index| index + 1)
                .unwrap_or(0);
            bindings.push((callback_index, sched_context_index));
            callback_index += 1;
        }
    }
    bindings
}

fn component_language(raw: &str) -> &'static str {
    match raw {
        "rust" | "Rust" => "Rust",
        "c" | "C" => "C",
        "cpp" | "c++" | "Cpp" => "Cpp",
        _ => "Rust",
    }
}

fn rust_crate_name(component_id: &str) -> Option<&str> {
    component_id
        .split("::")
        .next()
        .filter(|name| !name.is_empty())
}

fn rust_component_type_path(component_id: &str) -> Option<String> {
    let mut parts = component_id.split("::").filter(|part| !part.is_empty());
    let crate_name = parts.next()?;
    let module = parts.next()?;
    Some(format!("{crate_name}::{module}::Component"))
}

fn native_register_fn_name(component_id: &str) -> String {
    format!("register_native_component_{}", rust_ident(component_id))
}

fn native_symbol_fn_name(component_id: &str) -> String {
    format!("nros_native_symbol_{}", rust_ident(component_id))
}

fn rust_ident(raw: &str) -> String {
    let mut ident = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    while ident.contains("__") {
        ident = ident.replace("__", "_");
    }
    if ident
        .chars()
        .next()
        .is_none_or(|ch| !ch.is_ascii_alphabetic() && ch != '_')
    {
        ident.insert(0, '_');
    }
    ident
}

fn final_node_name(resolved_name: &str, namespace: &str) -> String {
    let trimmed = resolved_name.trim_matches('/');
    if trimmed.is_empty() {
        return "node".to_string();
    }
    let namespace = namespace.trim_matches('/');
    if !namespace.is_empty() {
        if let Some(stripped) = trimmed.strip_prefix(namespace) {
            let stripped = stripped.trim_matches('/');
            if !stripped.is_empty() {
                return stripped.to_string();
            }
        }
    }
    trimmed
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

fn render_parameter_value(value: &ParameterValue) -> Option<String> {
    match value {
        ParameterValue::Bool(value) => Some(format!("ParameterValue::Bool({value})")),
        ParameterValue::Integer(value) => Some(format!("ParameterValue::I64({value})")),
        ParameterValue::Float(value) => Some(format!("ParameterValue::F64({value:?})")),
        ParameterValue::String(value) => Some(format!("ParameterValue::Str({value:?})")),
        _ => None,
    }
}

fn sched_class(class: &SchedClass) -> &'static str {
    match class {
        SchedClass::BestEffort => "BestEffort",
        SchedClass::RealTime => "Fifo",
        SchedClass::TimeTriggered => "Fifo",
        SchedClass::Interrupt => "Fifo",
    }
}

fn priority(priority: Option<u8>) -> &'static str {
    match priority {
        Some(0..=63) => "BestEffort",
        Some(64..=191) => "Normal",
        Some(_) => "Critical",
        None => "Normal",
    }
}

fn deadline_policy(policy: &DeadlinePolicy) -> &'static str {
    match policy {
        DeadlinePolicy::Ignore => "Activated",
        DeadlinePolicy::Warn => "Activated",
        DeadlinePolicy::Skip => "Activated",
        DeadlinePolicy::Fault => "Activated",
    }
}

fn option_ms_to_us(value: Option<u64>) -> String {
    match value
        .and_then(|ms| ms.checked_mul(1_000))
        .and_then(|us| u32::try_from(us).ok())
    {
        Some(us) => format!("Some({us})"),
        None => "None".to_string(),
    }
}

fn stable_plan_id(plan: &NrosPlan) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for byte in plan.system.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}
