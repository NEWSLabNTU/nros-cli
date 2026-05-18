//! Draft host planner for Phase 126.C.

use super::{
    manifest::{ManifestArtifact, endpoint_requirements, load_manifest},
    names,
    params::{ParameterInputs, effective_parameters, load_toml_values},
    plan::{NrosPlan, PlanEntity},
    schema::InterfaceRef,
    workspace::{Workspace, unique_paths},
};
use eyre::{Context, Result, eyre};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct PlanOptions {
    pub system_pkg: String,
    pub workspace_root: PathBuf,
    pub launch_file: PathBuf,
    pub record_file: Option<PathBuf>,
    pub out_root: PathBuf,
    pub metadata_files: Vec<PathBuf>,
    pub manifest_files: Vec<PathBuf>,
    pub nros_toml_files: Vec<PathBuf>,
    pub launch_args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PlanningOutput {
    pub record_path: PathBuf,
    pub plan_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CheckReport {
    pub errors: usize,
    pub warnings: usize,
}

#[derive(Debug, Clone)]
struct JsonArtifact {
    path: PathBuf,
    value: Value,
}

pub fn plan_system(options: PlanOptions) -> Result<PlanningOutput> {
    fs::create_dir_all(&options.out_root)?;
    let metadata_dir = options.out_root.join("metadata");
    fs::create_dir_all(&metadata_dir)?;

    let workspace = Workspace::discover(&options.workspace_root)?;
    let launch_args = parse_launch_args(&options.launch_args)?;
    let record = load_or_parse_record(
        &options.launch_file,
        options.record_file.as_deref(),
        launch_args,
    )?;

    let record_path = options.out_root.join("record.json");
    fs::write(&record_path, serde_json::to_string_pretty(&record)?)?;

    let metadata_paths = metadata_paths(&options, &workspace, &metadata_dir);
    let metadata = load_json_artifacts(&metadata_paths, "source metadata")?;
    preserve_metadata(&metadata, &metadata_dir)?;

    let manifest_paths = if options.manifest_files.is_empty() {
        workspace.manifest_files()
    } else {
        unique_paths(options.manifest_files.clone())
    };
    let manifests = manifest_paths
        .iter()
        .map(|path| load_manifest(path))
        .collect::<Result<Vec<_>>>()?;

    let mut nros_toml = options.nros_toml_files.clone();
    if let Some(system_toml) = workspace.package_nros_toml(&options.system_pkg) {
        nros_toml.push(system_toml);
    }
    let overlays = load_toml_values(&unique_paths(nros_toml))?;

    let (instances, mut diagnostics) =
        build_instances(&record, &metadata, &workspace, &overlays, &record_path);
    diagnostics.extend(check_manifest_endpoints(
        &instances,
        &manifests,
        &metadata,
        &record_path,
    ));

    if diagnostics
        .iter()
        .any(|diag| diag.get("severity").and_then(Value::as_str) == Some("error"))
    {
        return Err(eyre!(
            "planning failed with {} error(s): {}",
            diagnostics
                .iter()
                .filter(|diag| diag.get("severity").and_then(Value::as_str) == Some("error"))
                .count(),
            diagnostics
                .iter()
                .filter(|diag| diag.get("severity").and_then(Value::as_str) == Some("error"))
                .map(diagnostic_summary)
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }

    let plan = schema_plan_json(&options, &record_path, &instances, &metadata);

    let plan_path = options.out_root.join("nros-plan.json");
    fs::write(&plan_path, serde_json::to_string_pretty(&plan)?)?;
    Ok(PlanningOutput {
        record_path,
        plan_path,
    })
}

pub fn check_plan_file(path: &Path) -> Result<CheckReport> {
    let raw = fs::read_to_string(path)
        .wrap_err_with(|| format!("failed to read plan {}", path.display()))?;
    let plan: NrosPlan = serde_json::from_str(&raw)
        .wrap_err_with(|| format!("invalid nros-plan.json schema {}", path.display()))?;
    let errors = validate_plan(&plan);
    if !errors.is_empty() {
        return Err(eyre!(
            "invalid nros-plan.json graph {}: {} error(s): {}",
            path.display(),
            errors.len(),
            errors.join("; ")
        ));
    }
    Ok(CheckReport {
        errors: 0,
        warnings: 0,
    })
}

fn validate_plan(plan: &NrosPlan) -> Vec<String> {
    let mut errors = Vec::new();
    let mut component_ids = HashSet::new();
    let mut instance_ids = HashSet::new();
    let mut sched_context_ids = HashSet::new();
    let mut interface_ids = HashSet::new();
    let mut component_lookup = HashSet::new();
    let mut sched_context_lookup = HashSet::new();
    let mut entity_lookup = HashSet::new();
    let mut interface_lookup = HashMap::new();

    for component in &plan.components {
        push_duplicate(
            &mut errors,
            "duplicate-component-id",
            &component.id,
            &mut component_ids,
        );
        component_lookup.insert(component.id.as_str());
    }
    for context in &plan.sched_contexts {
        push_duplicate(
            &mut errors,
            "duplicate-sched-context-id",
            &context.id,
            &mut sched_context_ids,
        );
        sched_context_lookup.insert(context.id.as_str());
    }
    for interface in &plan.interfaces {
        push_duplicate(
            &mut errors,
            "duplicate-interface-id",
            &interface.id,
            &mut interface_ids,
        );
        interface_lookup.insert(interface.id.as_str(), &interface.interface);
    }

    for instance in &plan.instances {
        push_duplicate(
            &mut errors,
            "duplicate-instance-id",
            &instance.id,
            &mut instance_ids,
        );
        if !component_lookup.contains(instance.component.as_str()) {
            errors.push(format!(
                "missing-component-reference: instance {} references {}",
                instance.id, instance.component
            ));
        }

        let mut node_ids = HashSet::new();
        let mut local_entity_ids = HashSet::new();
        let mut callback_ids = HashSet::new();
        for node in &instance.nodes {
            push_duplicate(&mut errors, "duplicate-node-id", &node.id, &mut node_ids);
            for entity in &node.entities {
                let entity_id = plan_entity_id(entity);
                push_duplicate(
                    &mut errors,
                    "duplicate-entity-id",
                    entity_id,
                    &mut local_entity_ids,
                );
                entity_lookup.insert(entity_id);
            }
        }
        for callback in &instance.callbacks {
            push_duplicate(
                &mut errors,
                "duplicate-callback-id",
                &callback.id,
                &mut callback_ids,
            );
            if !sched_context_lookup.contains(callback.sched_context.as_str()) {
                errors.push(format!(
                    "missing-sched-context: callback {} references {}",
                    callback.id, callback.sched_context
                ));
            }
        }
        for binding in &instance.sched_bindings {
            if !callback_ids.contains(binding.callback.as_str()) {
                errors.push(format!(
                    "missing-sched-callback: binding references {}",
                    binding.callback
                ));
            }
            if !sched_context_lookup.contains(binding.context.as_str()) {
                errors.push(format!(
                    "missing-sched-context: binding for {} references {}",
                    binding.callback, binding.context
                ));
            }
        }
        for parameter in &instance.parameters {
            if !node_ids.contains(parameter.node.as_str()) {
                errors.push(format!(
                    "missing-parameter-node: parameter {} references {}",
                    parameter.name, parameter.node
                ));
            }
        }
    }

    for interface in &plan.interfaces {
        for entity_id in &interface.used_by {
            if !entity_lookup.contains(entity_id.as_str()) {
                errors.push(format!(
                    "missing-interface-entity: interface {} references {}",
                    interface.id, entity_id
                ));
            }
        }
    }
    for instance in &plan.instances {
        for node in &instance.nodes {
            for entity in &node.entities {
                let Some(entity_interface) = plan_entity_interface(entity) else {
                    continue;
                };
                let entity_id = plan_entity_id(entity);
                let interface_id = interface_id(entity_interface);
                match interface_lookup.get(interface_id.as_str()) {
                    Some(table_interface) if *table_interface == entity_interface => {}
                    Some(_) => errors.push(format!(
                        "interface-ref-mismatch: entity {} uses {}",
                        entity_id, interface_id
                    )),
                    None => errors.push(format!(
                        "missing-interface-ref: entity {} uses {}",
                        entity_id, interface_id
                    )),
                }
                if !plan.interfaces.iter().any(|interface| {
                    interface.id == interface_id
                        && interface.used_by.iter().any(|id| id == entity_id)
                }) {
                    errors.push(format!(
                        "missing-interface-usage: entity {} not listed under {}",
                        entity_id, interface_id
                    ));
                }
            }
        }
    }

    errors
}

fn push_duplicate<'a>(
    errors: &mut Vec<String>,
    code: &str,
    id: &'a str,
    seen: &mut HashSet<&'a str>,
) {
    if !seen.insert(id) {
        errors.push(format!("{code}: {id}"));
    }
}

fn plan_entity_id(entity: &PlanEntity) -> &str {
    match entity {
        PlanEntity::Publisher { id, .. }
        | PlanEntity::Subscriber { id, .. }
        | PlanEntity::Timer { id, .. }
        | PlanEntity::ServiceServer { id, .. }
        | PlanEntity::ServiceClient { id, .. }
        | PlanEntity::ActionServer { id, .. }
        | PlanEntity::ActionClient { id, .. } => id,
    }
}

fn plan_entity_interface(entity: &PlanEntity) -> Option<&InterfaceRef> {
    match entity {
        PlanEntity::Publisher { interface, .. }
        | PlanEntity::Subscriber { interface, .. }
        | PlanEntity::ServiceServer { interface, .. }
        | PlanEntity::ServiceClient { interface, .. }
        | PlanEntity::ActionServer { interface, .. }
        | PlanEntity::ActionClient { interface, .. } => Some(interface),
        PlanEntity::Timer { .. } => None,
    }
}

fn interface_id(interface: &InterfaceRef) -> String {
    format!("{}/{}", interface.package, interface.name)
}

fn parse_launch_args(args: &[String]) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for arg in args {
        let Some((key, value)) = arg.split_once(":=").or_else(|| arg.split_once('=')) else {
            return Err(eyre!(
                "invalid launch argument `{arg}`: expected name:=value or name=value"
            ));
        };
        out.insert(key.to_string(), value.to_string());
    }
    Ok(out)
}

fn load_or_parse_record(
    launch_file: &Path,
    record_file: Option<&Path>,
    launch_args: HashMap<String, String>,
) -> Result<Value> {
    if let Some(record_file) = record_file {
        let raw = fs::read_to_string(record_file)
            .wrap_err_with(|| format!("failed to read record {}", record_file.display()))?;
        return serde_json::from_str(&raw)
            .wrap_err_with(|| format!("invalid record JSON {}", record_file.display()));
    }
    parse_launch_file_record(launch_file, launch_args)
}

#[cfg(feature = "play-launch-parser")]
fn parse_launch_file_record(
    launch_file: &Path,
    launch_args: HashMap<String, String>,
) -> Result<Value> {
    let record =
        play_launch_parser::parse_launch_file(launch_file, launch_args).map_err(|err| {
            eyre!(
                "failed to parse launch file {}: {err}",
                launch_file.display()
            )
        })?;
    Ok(serde_json::to_value(record)?)
}

#[cfg(not(feature = "play-launch-parser"))]
fn parse_launch_file_record(
    launch_file: &Path,
    _launch_args: HashMap<String, String>,
) -> Result<Value> {
    Err(eyre!(
        "play_launch_parser adapter is disabled for this build; pass --record <record.json> or build nros-cli-core with feature `play-launch-parser` (launch file: {})",
        launch_file.display()
    ))
}

fn record_array<'a>(record: &'a Value, key: &str) -> &'a [Value] {
    record
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn metadata_paths(
    options: &PlanOptions,
    workspace: &Workspace,
    metadata_dir: &Path,
) -> Vec<PathBuf> {
    let mut paths = options.metadata_files.clone();
    paths.extend(workspace.source_metadata_files());
    if metadata_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(metadata_dir) {
            paths.extend(
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json")),
            );
        }
    }
    unique_paths(paths)
}

fn load_json_artifacts(paths: &[PathBuf], label: &str) -> Result<Vec<JsonArtifact>> {
    paths
        .iter()
        .map(|path| {
            let raw = fs::read_to_string(path)
                .wrap_err_with(|| format!("failed to read {label} {}", path.display()))?;
            let value = serde_json::from_str(&raw)
                .wrap_err_with(|| format!("invalid {label} JSON {}", path.display()))?;
            Ok(JsonArtifact {
                path: path.clone(),
                value,
            })
        })
        .collect()
}

fn preserve_metadata(metadata: &[JsonArtifact], metadata_dir: &Path) -> Result<()> {
    for artifact in metadata {
        let Some(file_name) = artifact.path.file_name() else {
            continue;
        };
        let dest = metadata_dir.join(file_name);
        if dest != artifact.path {
            fs::write(dest, serde_json::to_string_pretty(&artifact.value)?)?;
        }
    }
    Ok(())
}

fn schema_plan_json(
    options: &PlanOptions,
    record_path: &Path,
    instances: &[Value],
    metadata: &[JsonArtifact],
) -> Value {
    let components = schema_components(metadata);
    let plan_instances = instances.iter().map(schema_instance).collect::<Vec<_>>();
    let interfaces = schema_interfaces(&plan_instances);
    json!({
        "version": 1,
        "system": options.system_pkg,
        "trace": {
            "system_config": options.nros_toml_files.first().map(|p| p.display().to_string()).unwrap_or_else(|| "nros.toml".to_string()),
            "launch_record": record_path.display().to_string(),
            "generated_by": "nros plan",
        },
        "components": components,
        "instances": plan_instances,
        "interfaces": interfaces,
        "sched_contexts": [{
            "id": "default_executor",
            "executor": "single_threaded",
            "class": "best_effort",
            "priority": null,
            "period_ms": null,
            "budget_ms": null,
            "deadline_ms": null,
            "deadline_policy": "ignore",
            "stack_size": null,
            "core": null,
            "task": null,
        }],
        "build": {
            "target": "x86_64-unknown-linux-gnu",
            "board": "native",
            "rmw": "zenoh",
            "profile": "debug",
            "features": [],
            "cfg": {},
        },
    })
}

fn schema_components(metadata: &[JsonArtifact]) -> Vec<Value> {
    metadata
        .iter()
        .map(|artifact| {
            let package = string_field(&artifact.value, &["package"]).unwrap_or("unknown");
            let component =
                string_field(&artifact.value, &["component", "executable"]).unwrap_or("unknown");
            let language = string_field(&artifact.value, &["language"]).unwrap_or("rust");
            json!({
                "id": format!("{package}::{component}"),
                "package": package,
                "component": component,
                "language": language,
                "source_metadata": artifact.path.display().to_string(),
                "component_config": null,
            })
        })
        .collect()
}

fn schema_instance(instance: &Value) -> Value {
    let id = instance
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("instance");
    let package = instance
        .get("package")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let executable = instance
        .get("executable")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let namespace = instance
        .get("namespace")
        .and_then(Value::as_str)
        .unwrap_or("/");
    let launch_name = instance
        .get("node_name")
        .and_then(Value::as_str)
        .unwrap_or(executable);
    let source_nodes = instance
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            vec![json!({
                "id": "node",
                "resolved_name": launch_name,
                "namespace": namespace,
            })]
        });
    let raw_entities = instance
        .get("entities")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let nodes = schema_nodes(id, &source_nodes, &raw_entities);
    let callbacks = schema_callbacks(id, instance.get("callbacks"));
    let sched_bindings = schema_sched_bindings(&callbacks);
    let default_source_node = source_nodes
        .first()
        .and_then(|node| node.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("node");
    json!({
        "id": id,
        "component": format!("{package}::{executable}"),
        "package": package,
        "executable": executable,
        "launch_name": launch_name,
        "namespace": namespace,
        "remaps": schema_remaps(instance.get("remaps")),
        "nodes": nodes,
        "callbacks": callbacks,
        "parameters": schema_parameters(id, default_source_node, instance.get("parameters")),
        "sched_bindings": sched_bindings,
        "trace": {
            "launch_record_entity": format!("record://{id}"),
            "source_metadata": instance.get("source_metadata").and_then(Value::as_str).unwrap_or(""),
        },
    })
}

fn schema_nodes(instance_id: &str, source_nodes: &[Value], entities: &[Value]) -> Vec<Value> {
    source_nodes
        .iter()
        .map(|node| {
            let source_node = node.get("id").and_then(Value::as_str).unwrap_or("node");
            let node_entities = entities
                .iter()
                .filter(|entity| {
                    entity
                        .get("source_node")
                        .and_then(Value::as_str)
                        .unwrap_or("node")
                        == source_node
                })
                .filter_map(|entity| schema_entity(instance_id, entity))
                .collect::<Vec<_>>();
            json!({
                "id": format!("{instance_id}/{source_node}"),
                "source_node": source_node,
                "resolved_name": node.get("resolved_name").and_then(Value::as_str).unwrap_or(""),
                "namespace": node.get("namespace").and_then(Value::as_str).unwrap_or("/"),
                "entities": node_entities,
            })
        })
        .collect()
}

fn schema_callbacks(instance_id: &str, value: Option<&Value>) -> Vec<Value> {
    let Some(Value::Array(callbacks)) = value else {
        return Vec::new();
    };
    callbacks
        .iter()
        .filter_map(|callback| {
            let source_callback = callback.get("id").and_then(Value::as_str)?;
            if source_callback.is_empty() {
                return None;
            }
            let source = callback.get("source").cloned().unwrap_or_else(|| {
                json!({
                    "artifact": "source-metadata.json",
                    "line": null,
                    "column": null,
                })
            });
            Some(json!({
                "id": format!("{instance_id}/{source_callback}"),
                "source_callback": source_callback,
                "group": callback.get("group").and_then(Value::as_str).unwrap_or("default"),
                "sched_context": "default_executor",
                "source": source,
            }))
        })
        .collect()
}

fn schema_sched_bindings(callbacks: &[Value]) -> Vec<Value> {
    callbacks
        .iter()
        .filter_map(|callback| {
            let id = callback.get("id").and_then(Value::as_str)?;
            Some(json!({
                "callback": id,
                "context": "default_executor",
                "priority": null,
                "source": "source_metadata",
            }))
        })
        .collect()
}

fn schema_remaps(value: Option<&Value>) -> Vec<Value> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            Value::Array(pair) if pair.len() == 2 => Some(json!({
                "from": pair[0].as_str().unwrap_or_default(),
                "to": pair[1].as_str().unwrap_or_default(),
            })),
            _ => None,
        })
        .collect()
}

fn schema_entity(instance_id: &str, entity: &Value) -> Option<Value> {
    let role = entity.get("role").and_then(Value::as_str)?;
    let source_entity = entity
        .get("source_id")
        .and_then(Value::as_str)
        .unwrap_or("entity");
    let id = format!("{instance_id}/{source_entity}");
    let trace = json!({
        "source_artifact": {
            "artifact": entity.get("source_artifact").and_then(Value::as_str).unwrap_or("source-metadata.json"),
            "line": null,
            "column": null,
        },
        "manifest_endpoint": null,
    });
    match role {
        "publisher" => Some(json!({
            "role": role,
            "id": id,
            "source_entity": source_entity,
            "resolved_name": entity.get("resolved_name").and_then(Value::as_str).unwrap_or(""),
            "interface": schema_interface(entity.get("type"))?,
            "qos": schema_qos(entity.get("qos")),
            "trace": trace,
        })),
        "subscriber" => Some(json!({
            "role": role,
            "id": id,
            "source_entity": source_entity,
            "callback": entity.get("callback"),
            "resolved_name": entity.get("resolved_name").and_then(Value::as_str).unwrap_or(""),
            "interface": schema_interface(entity.get("type"))?,
            "qos": schema_qos(entity.get("qos")),
            "trace": trace,
        })),
        "timer" => Some(json!({
            "role": "timer",
            "id": id,
            "source_entity": source_entity,
            "callback": entity.get("callback"),
            "period_ms": entity.get("period_ms").and_then(Value::as_u64).unwrap_or(0),
            "trace": trace,
        })),
        "service_server" | "action_server" => Some(json!({
            "role": role,
            "id": id,
            "source_entity": source_entity,
            "callback": entity.get("callback"),
            "resolved_name": entity.get("resolved_name").and_then(Value::as_str).unwrap_or(""),
            "interface": schema_interface(entity.get("type"))?,
            "qos": null,
            "trace": trace,
        })),
        "service_client" | "action_client" => Some(json!({
            "role": role,
            "id": id,
            "source_entity": source_entity,
            "resolved_name": entity.get("resolved_name").and_then(Value::as_str).unwrap_or(""),
            "interface": schema_interface(entity.get("type"))?,
            "qos": null,
            "trace": trace,
        })),
        _ => None,
    }
}

fn schema_interface(value: Option<&Value>) -> Option<Value> {
    match value? {
        Value::Object(map) => Some(json!({
            "package": map.get("package").and_then(Value::as_str).unwrap_or(""),
            "name": map.get("name").and_then(Value::as_str).unwrap_or(""),
            "kind": map.get("kind").and_then(Value::as_str).unwrap_or("message"),
        })),
        Value::String(raw) => {
            let (package, name) = raw.split_once('/').unwrap_or(("", raw));
            Some(json!({
                "package": package,
                "name": name,
                "kind": if name.starts_with("srv/") {
                    "service"
                } else if name.starts_with("action/") {
                    "action"
                } else {
                    "message"
                },
            }))
        }
        _ => None,
    }
}

fn schema_qos(value: Option<&Value>) -> Value {
    if let Some(value) = value.filter(|value| !value.is_null()) {
        return value.clone();
    }
    json!({
        "reliability": "system_default",
        "durability": "system_default",
        "history": "system_default",
        "depth": 0,
        "deadline_ms": null,
        "lifespan_ms": null,
        "liveliness": "system_default",
        "liveliness_lease_duration_ms": null,
        "extensions": {},
    })
}

fn schema_parameters(
    instance_id: &str,
    default_source_node: &str,
    value: Option<&Value>,
) -> Vec<Value> {
    let Some(Value::Object(map)) = value else {
        return Vec::new();
    };
    map.iter()
        .filter(|(name, _)| name.as_str() != "parameter_files")
        .map(|(name, value)| {
            json!({
                "node": format!("{instance_id}/{default_source_node}"),
                "name": name,
                "value": schema_parameter_value(value),
                "source": {
                    "kind": "launch",
                    "artifact": "launch",
                },
            })
        })
        .collect()
}

fn schema_parameter_value(value: &Value) -> Value {
    match value {
        Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
        Value::Array(items) => {
            if items.iter().all(Value::is_boolean)
                || items.iter().all(|v| v.as_i64().is_some())
                || items.iter().all(|v| v.as_f64().is_some())
                || items.iter().all(Value::is_string)
            {
                value.clone()
            } else {
                Value::String(value.to_string())
            }
        }
        _ => Value::String(value.to_string()),
    }
}

fn schema_interfaces(instances: &[Value]) -> Vec<Value> {
    let mut used: std::collections::BTreeMap<String, (Value, Vec<String>)> =
        std::collections::BTreeMap::new();
    for entity in instances
        .iter()
        .flat_map(|instance| instance.get("nodes").and_then(Value::as_array))
        .flatten()
        .flat_map(|node| node.get("entities").and_then(Value::as_array))
        .flatten()
    {
        let Some(interface) = entity.get("interface") else {
            continue;
        };
        let package = interface
            .get("package")
            .and_then(Value::as_str)
            .unwrap_or("");
        let name = interface.get("name").and_then(Value::as_str).unwrap_or("");
        let key = format!("{package}/{name}");
        used.entry(key)
            .or_insert_with(|| (interface.clone(), Vec::new()))
            .1
            .push(
                entity
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            );
    }
    used.into_iter()
        .map(|(id, (interface, used_by))| {
            json!({
                "id": id,
                "interface": interface,
                "used_by": used_by,
            })
        })
        .collect()
}

fn build_instances(
    record: &Value,
    metadata: &[JsonArtifact],
    workspace: &Workspace,
    overlays: &[Value],
    record_path: &Path,
) -> (Vec<Value>, Vec<Value>) {
    let mut counts = HashMap::<(String, String), usize>::new();
    let mut diagnostics = Vec::new();
    let mut instances = Vec::new();

    for node in record_array(record, "node") {
        let package = string_field(node, &["package"]).unwrap_or_default();
        if package.is_empty() {
            diagnostics.push(diagnostic(
                "error",
                "missing-package",
                "launch node has no package",
                None,
                None,
                None,
                record_path,
            ));
            continue;
        }
        let executable = string_field(node, &["executable"]).unwrap_or_default();
        let params = pairs_field(node, "params");
        let remaps = pairs_field(node, "remaps");
        let param_files = string_list_field(node, "params_files");
        instances.push(build_node_instance(
            package,
            executable,
            string_field(node, &["name"]),
            string_field(node, &["namespace"]),
            &params,
            &param_files,
            &remaps,
            "node",
            metadata,
            workspace,
            overlays,
            record_path,
            &mut counts,
            &mut diagnostics,
        ));
    }

    for load_node in record_array(record, "load_node") {
        let package = string_field(load_node, &["package"]).unwrap_or_default();
        let plugin = string_field(load_node, &["plugin"]).unwrap_or_default();
        let executable = plugin.split("::").last().unwrap_or(plugin);
        let params = pairs_field(load_node, "params");
        let remaps = pairs_field(load_node, "remaps");
        instances.push(build_node_instance(
            package,
            executable,
            string_field(load_node, &["node_name"]),
            string_field(load_node, &["namespace"]),
            &params,
            &[],
            &remaps,
            "load_node",
            metadata,
            workspace,
            overlays,
            record_path,
            &mut counts,
            &mut diagnostics,
        ));
    }

    (instances, diagnostics)
}

#[allow(clippy::too_many_arguments)]
fn build_node_instance(
    package: &str,
    executable: &str,
    name: Option<&str>,
    namespace: Option<&str>,
    params: &[(String, String)],
    param_files: &[String],
    remaps: &[(String, String)],
    launch_kind: &str,
    metadata: &[JsonArtifact],
    workspace: &Workspace,
    overlays: &[Value],
    record_path: &Path,
    counts: &mut HashMap<(String, String), usize>,
    diagnostics: &mut Vec<Value>,
) -> Value {
    let index = next_instance_index(counts, package, executable);
    let instance_id = format!(
        "{}.{}.{}",
        sanitize_id(package),
        sanitize_id(executable),
        index
    );
    let node_name = names::node_fqn(namespace, name, executable);
    let namespace = names::normalize_namespace(namespace);
    let source_metadata = find_source_metadata(metadata, package, executable);
    if source_metadata.is_none() {
        diagnostics.push(diagnostic(
            "error",
            "missing-source-metadata",
            format!("missing source metadata for {package}/{executable}"),
            Some(package),
            Some(&instance_id),
            None,
            record_path,
        ));
    }
    if let Some(artifact) = source_metadata {
        let language = artifact
            .value
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or("");
        let exported = artifact
            .value
            .get("exported_symbol")
            .and_then(Value::as_str)
            .filter(|symbol| !symbol.is_empty());
        if language == "rust" && exported.is_none() {
            diagnostics.push(diagnostic(
                "error",
                "missing-component-export",
                format!(
                    "rust component {package}/{executable} declares no exported_symbol — \
                     add `nros::component_export!` (or `#[nros::component]`) so the planner \
                     can wire it into the generated runtime"
                ),
                Some(package),
                Some(&instance_id),
                None,
                &artifact.path,
            ));
        }
    }

    let package_nros = workspace
        .package_nros_toml(package)
        .and_then(|path| load_toml_values(&[path]).ok())
        .and_then(|mut values| values.pop());
    let parameters = effective_parameters(ParameterInputs {
        source_metadata: source_metadata.map(|artifact| &artifact.value),
        package_nros: package_nros.as_ref(),
        launch_params: params,
        param_files,
        overlays,
    });
    let entities = source_metadata
        .map(|artifact| {
            source_entities(
                &artifact.value,
                &artifact.path,
                &namespace,
                node_name.trim_start_matches('/'),
                remaps,
            )
        })
        .unwrap_or_default();
    let nodes = source_metadata
        .map(|artifact| {
            source_nodes(
                &artifact.value,
                &namespace,
                node_name.trim_start_matches('/'),
            )
        })
        .unwrap_or_else(|| {
            vec![json!({
                "id": "node",
                "resolved_name": node_name,
                "namespace": namespace,
            })]
        });
    let callbacks = source_metadata
        .map(|artifact| source_callbacks(&artifact.value))
        .unwrap_or_default();
    if let Some(artifact) = source_metadata {
        diagnostics.extend(check_source_metadata_links(
            &artifact.value,
            &artifact.path,
            package,
            &instance_id,
        ));
    }

    json!({
        "id": instance_id,
        "telemetry_id": format!("{package}::{executable}#{index}"),
        "package": package,
        "executable": executable,
        "launch_kind": launch_kind,
        "node_name": node_name,
        "namespace": namespace,
        "remaps": remaps,
        "parameters": parameters,
        "source_metadata": source_metadata.map(|artifact| artifact.path.to_string_lossy().to_string()),
        "nodes": nodes,
        "entities": entities,
        "callbacks": callbacks,
    })
}

fn check_source_metadata_links(
    metadata: &Value,
    path: &Path,
    package: &str,
    instance_id: &str,
) -> Vec<Value> {
    let entity_ids = source_entity_ids(metadata);
    let callback_ids = source_callback_ids(metadata);
    let mut diagnostics = Vec::new();

    if let Some(callbacks) = metadata.get("callbacks").and_then(Value::as_array) {
        for callback in callbacks {
            let callback_id = callback.get("id").and_then(Value::as_str).unwrap_or("");
            let Some(effects) = callback.get("effects").and_then(Value::as_array) else {
                continue;
            };
            for effect in effects {
                let entity_id = effect.get("entity").and_then(Value::as_str).unwrap_or("");
                if !entity_id.is_empty() && !entity_ids.contains(entity_id) {
                    diagnostics.push(diagnostic(
                        "error",
                        "callback-effect-unknown-entity",
                        format!(
                            "callback {callback_id} effect references unknown entity {entity_id}"
                        ),
                        Some(package),
                        Some(instance_id),
                        Some(entity_id),
                        path,
                    ));
                }
            }
        }
    }

    for (entity_id, callback_id) in source_entity_callback_refs(metadata) {
        if !callback_id.is_empty() && !callback_ids.contains(callback_id.as_str()) {
            diagnostics.push(diagnostic(
                "error",
                "entity-callback-missing",
                format!("entity {entity_id} references missing callback {callback_id}"),
                Some(package),
                Some(instance_id),
                Some(&entity_id),
                path,
            ));
        }
    }

    diagnostics
}

fn source_entity_ids(metadata: &Value) -> HashSet<&str> {
    let mut ids = HashSet::new();
    collect_source_entity_ids(metadata.get("entities"), &mut ids);
    collect_source_entity_ids(metadata.get("publishers"), &mut ids);
    collect_source_entity_ids(metadata.get("subscriptions"), &mut ids);
    collect_source_entity_ids(metadata.get("subscribers"), &mut ids);
    collect_source_entity_ids(metadata.get("services"), &mut ids);
    collect_source_entity_ids(metadata.get("clients"), &mut ids);
    collect_source_entity_ids(metadata.get("actions"), &mut ids);
    collect_source_entity_ids(metadata.get("parameters"), &mut ids);
    if let Some(nodes) = metadata.get("nodes").and_then(Value::as_array) {
        for node in nodes {
            collect_source_entity_ids(node.get("publishers"), &mut ids);
            collect_source_entity_ids(node.get("subscribers"), &mut ids);
            collect_source_entity_ids(node.get("timers"), &mut ids);
            collect_source_entity_ids(node.get("services"), &mut ids);
            collect_source_entity_ids(node.get("actions"), &mut ids);
            collect_source_entity_ids(node.get("parameters"), &mut ids);
        }
    }
    ids
}

fn collect_source_entity_ids<'a>(value: Option<&'a Value>, ids: &mut HashSet<&'a str>) {
    let Some(items) = value.and_then(Value::as_array) else {
        return;
    };
    for item in items {
        if let Some(id) = item
            .get("id")
            .or_else(|| item.get("entity"))
            .and_then(Value::as_str)
        {
            ids.insert(id);
        }
    }
}

fn source_callback_ids(metadata: &Value) -> HashSet<&str> {
    metadata
        .get("callbacks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|callback| callback.get("id").and_then(Value::as_str))
        .collect()
}

fn source_entity_callback_refs(metadata: &Value) -> Vec<(String, String)> {
    let mut refs = Vec::new();
    collect_source_entity_callback_refs(metadata.get("entities"), &mut refs);
    collect_source_entity_callback_refs(metadata.get("subscriptions"), &mut refs);
    collect_source_entity_callback_refs(metadata.get("subscribers"), &mut refs);
    collect_source_entity_callback_refs(metadata.get("services"), &mut refs);
    collect_source_entity_callback_refs(metadata.get("actions"), &mut refs);
    if let Some(nodes) = metadata.get("nodes").and_then(Value::as_array) {
        for node in nodes {
            collect_source_entity_callback_refs(node.get("subscribers"), &mut refs);
            collect_source_entity_callback_refs(node.get("timers"), &mut refs);
            collect_source_entity_callback_refs(node.get("services"), &mut refs);
            collect_source_entity_callback_refs(node.get("actions"), &mut refs);
        }
    }
    refs
}

fn collect_source_entity_callback_refs(value: Option<&Value>, refs: &mut Vec<(String, String)>) {
    let Some(items) = value.and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let entity_id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        for key in [
            "callback",
            "goal_callback",
            "cancel_callback",
            "accepted_callback",
        ] {
            let Some(callback_id) = item.get(key).and_then(Value::as_str) else {
                continue;
            };
            refs.push((entity_id.clone(), callback_id.to_string()));
        }
    }
}

fn source_callbacks(metadata: &Value) -> Vec<Value> {
    metadata
        .get("callbacks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn source_nodes(metadata: &Value, launch_namespace: &str, launch_node_name: &str) -> Vec<Value> {
    let Some(nodes) = metadata.get("nodes").and_then(Value::as_array) else {
        return vec![json!({
            "id": "node",
            "resolved_name": names::node_fqn(Some(launch_namespace), Some(launch_node_name), launch_node_name),
            "namespace": launch_namespace,
        })];
    };
    let single_node = nodes.len() == 1;
    nodes
        .iter()
        .map(|node| {
            let source_node = node.get("id").and_then(Value::as_str).unwrap_or("node");
            let metadata_namespace = node
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or(launch_namespace);
            let source_name = source_name_value(node.get("unresolved_name"));
            let resolved_name = if single_node {
                names::node_fqn(
                    Some(launch_namespace),
                    Some(launch_node_name),
                    launch_node_name,
                )
            } else {
                names::node_fqn(Some(metadata_namespace), Some(source_name), source_node)
            };
            let namespace = node_namespace(&resolved_name);
            json!({
                "id": source_node,
                "resolved_name": resolved_name,
                "namespace": namespace,
            })
        })
        .collect()
}

fn node_namespace(resolved_name: &str) -> String {
    let Some((namespace, _)) = resolved_name.rsplit_once('/') else {
        return "/".to_string();
    };
    if namespace.is_empty() {
        "/".to_string()
    } else {
        namespace.to_string()
    }
}

fn source_entities(
    metadata: &Value,
    path: &Path,
    namespace: &str,
    node_name: &str,
    remaps: &[(String, String)],
) -> Vec<Value> {
    let mut out = Vec::new();
    collect_schema_nodes(
        metadata.get("nodes"),
        path,
        namespace,
        node_name,
        remaps,
        &mut out,
    );
    collect_entity_array(
        metadata.get("entities"),
        "entity",
        path,
        namespace,
        node_name,
        remaps,
        &mut out,
    );
    collect_entity_array(
        metadata.get("publishers"),
        "publisher",
        path,
        namespace,
        node_name,
        remaps,
        &mut out,
    );
    collect_entity_array(
        metadata.get("subscriptions"),
        "subscriber",
        path,
        namespace,
        node_name,
        remaps,
        &mut out,
    );
    collect_entity_array(
        metadata.get("subscribers"),
        "subscriber",
        path,
        namespace,
        node_name,
        remaps,
        &mut out,
    );
    collect_entity_array(
        metadata.get("services"),
        "service_server",
        path,
        namespace,
        node_name,
        remaps,
        &mut out,
    );
    collect_entity_array(
        metadata.get("clients"),
        "service_client",
        path,
        namespace,
        node_name,
        remaps,
        &mut out,
    );
    collect_entity_array(
        metadata.get("actions"),
        "action",
        path,
        namespace,
        node_name,
        remaps,
        &mut out,
    );
    out
}

fn collect_schema_nodes(
    value: Option<&Value>,
    path: &Path,
    namespace: &str,
    node_name: &str,
    remaps: &[(String, String)],
    out: &mut Vec<Value>,
) {
    let Some(Value::Array(nodes)) = value else {
        return;
    };
    let single_node = nodes.len() == 1;
    for node in nodes {
        let source_node = node.get("id").and_then(Value::as_str).unwrap_or("node");
        let metadata_namespace = node
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or(namespace);
        let metadata_node_name = if single_node {
            node_name
        } else {
            source_name_value(node.get("unresolved_name"))
        };
        collect_schema_endpoint_array(
            node.get("publishers"),
            "publisher",
            "unresolved_topic",
            path,
            source_node,
            metadata_namespace,
            metadata_node_name,
            remaps,
            out,
        );
        collect_schema_endpoint_array(
            node.get("subscribers"),
            "subscriber",
            "unresolved_topic",
            path,
            source_node,
            metadata_namespace,
            metadata_node_name,
            remaps,
            out,
        );
        collect_schema_endpoint_array(
            node.get("services"),
            "service_server",
            "unresolved_name",
            path,
            source_node,
            metadata_namespace,
            metadata_node_name,
            remaps,
            out,
        );
        collect_schema_endpoint_array(
            node.get("actions"),
            "action_server",
            "unresolved_name",
            path,
            source_node,
            metadata_namespace,
            metadata_node_name,
            remaps,
            out,
        );
        collect_schema_timer_array(node.get("timers"), path, source_node, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_schema_endpoint_array(
    value: Option<&Value>,
    role: &str,
    name_key: &str,
    path: &Path,
    source_node: &str,
    namespace: &str,
    node_name: &str,
    remaps: &[(String, String)],
    out: &mut Vec<Value>,
) {
    let Some(Value::Array(items)) = value else {
        return;
    };
    for item in items {
        let source_name = source_name_value(item.get(name_key));
        let resolved = names::resolve_entity_name(namespace, node_name, source_name, remaps);
        out.push(json!({
            "source_artifact": path,
            "source_node": source_node,
            "source_id": item.get("id"),
            "role": role,
            "source_name": resolved.source,
            "source_name_kind": source_name_kind(item.get(name_key)),
            "resolved_name": resolved.resolved,
            "remapped_from": resolved.remapped_from,
            "type": item.get("interface"),
            "qos": item.get("qos"),
            "callback": item.get("callback")
                .or_else(|| item.get("goal_callback")),
        }));
    }
}

fn collect_schema_timer_array(
    value: Option<&Value>,
    path: &Path,
    source_node: &str,
    out: &mut Vec<Value>,
) {
    let Some(Value::Array(items)) = value else {
        return;
    };
    for item in items {
        out.push(json!({
            "source_artifact": path,
            "source_node": source_node,
            "source_id": item.get("id"),
            "role": "timer",
            "period_ms": item.get("period_ms"),
            "callback": item.get("callback"),
        }));
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_entity_array(
    value: Option<&Value>,
    default_role: &str,
    path: &Path,
    namespace: &str,
    node_name: &str,
    remaps: &[(String, String)],
    out: &mut Vec<Value>,
) {
    let Some(Value::Array(items)) = value else {
        return;
    };
    for item in items {
        let role = item
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or(default_role);
        let source_name = item
            .get("name")
            .or_else(|| item.get("topic"))
            .or_else(|| item.get("service"))
            .or_else(|| item.get("action"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let resolved = names::resolve_entity_name(namespace, node_name, source_name, remaps);
        out.push(json!({
            "source_artifact": path,
            "source_node": "node",
            "source_id": item.get("id"),
            "role": normalize_role(role),
            "source_name": resolved.source,
            "source_name_kind": infer_source_name_kind(source_name),
            "resolved_name": resolved.resolved,
            "remapped_from": resolved.remapped_from,
            "type": item.get("type")
                .or_else(|| item.get("interface_type"))
                .or_else(|| item.get("message_type")),
        }));
    }
}

fn source_name_value(value: Option<&Value>) -> &str {
    match value {
        Some(Value::String(name)) => name,
        Some(Value::Object(map)) => map.get("value").and_then(Value::as_str).unwrap_or(""),
        _ => "",
    }
}

fn source_name_kind(value: Option<&Value>) -> &str {
    match value {
        Some(Value::Object(map)) => map
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_else(|| infer_source_name_kind(source_name_value(value))),
        Some(Value::String(name)) => infer_source_name_kind(name),
        _ => "relative",
    }
}

fn infer_source_name_kind(name: &str) -> &str {
    if name == "~" || name.starts_with("~/") {
        "private"
    } else if name.starts_with('/') {
        "absolute"
    } else {
        "relative"
    }
}

fn check_manifest_endpoints(
    instances: &[Value],
    manifests: &[ManifestArtifact],
    metadata: &[JsonArtifact],
    record_path: &Path,
) -> Vec<Value> {
    let mut diagnostics = Vec::new();
    if manifests.is_empty() {
        diagnostics.push(diagnostic(
            "warning",
            "missing-launch-manifest",
            "no ROS launch manifest files were loaded",
            None,
            None,
            None,
            record_path,
        ));
        return diagnostics;
    }
    let requirements = endpoint_requirements(manifests);
    for requirement in &requirements {
        if !entity_matches_requirement(instances, requirement) {
            diagnostics.push(diagnostic(
                "error",
                "manifest-endpoint-unmatched",
                format!(
                    "manifest endpoint did not match source metadata: role={} name={} type={}",
                    requirement
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    requirement
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    requirement
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                ),
                None,
                None,
                Some(&artifact_list(metadata)),
                requirement
                    .get("source_artifact")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .as_deref()
                    .unwrap_or(record_path),
            ));
        }
    }
    diagnostics.extend(check_metadata_entities_in_manifest(
        instances,
        &requirements,
        record_path,
    ));
    diagnostics
}

fn check_metadata_entities_in_manifest(
    instances: &[Value],
    requirements: &[Value],
    record_path: &Path,
) -> Vec<Value> {
    let mut diagnostics = Vec::new();
    for instance in instances {
        let package = instance.get("package").and_then(Value::as_str);
        let instance_id = instance.get("id").and_then(Value::as_str);
        let Some(entities) = instance.get("entities").and_then(Value::as_array) else {
            continue;
        };
        for entity in entities {
            let role = entity.get("role").and_then(Value::as_str).unwrap_or("");
            if !is_manifest_endpoint_role(role) {
                continue;
            }
            if requirements
                .iter()
                .any(|requirement| entity_matches_single_requirement(instance, entity, requirement))
            {
                continue;
            }
            diagnostics.push(diagnostic(
                "error",
                "metadata-entity-unmatched",
                format!(
                    "source metadata entity is not covered by launch manifest: role={} name={} type={}",
                    role,
                    entity
                        .get("resolved_name")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    entity_type_display(entity)
                ),
                package,
                instance_id,
                entity.get("source_id").and_then(Value::as_str),
                entity
                    .get("source_artifact")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .as_deref()
                    .unwrap_or(record_path),
            ));
        }
    }
    diagnostics
}

fn is_manifest_endpoint_role(role: &str) -> bool {
    matches!(
        role,
        "publisher"
            | "subscriber"
            | "service_server"
            | "service_client"
            | "action_server"
            | "action_client"
    )
}

fn entity_matches_requirement(instances: &[Value], requirement: &Value) -> bool {
    instances
        .iter()
        .filter(|instance| requirement_node_matches(instance, requirement))
        .any(|instance| {
            instance
                .get("entities")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|entity| entity_matches_single_requirement(instance, entity, requirement))
        })
}

fn entity_matches_single_requirement(
    instance: &Value,
    entity: &Value,
    requirement: &Value,
) -> bool {
    if !requirement_node_matches(instance, requirement) {
        return false;
    }
    let role = requirement
        .get("role")
        .and_then(Value::as_str)
        .map(normalize_role)
        .unwrap_or_default();
    let name = requirement
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let interface_type = requirement.get("type").and_then(Value::as_str);
    entity.get("role").and_then(Value::as_str) == Some(role.as_str())
        && endpoint_name_matches(entity, name)
        && interface_type.is_none_or(|ty| entity_type_matches(entity, ty))
}

fn requirement_node_matches(instance: &Value, requirement: &Value) -> bool {
    let Some(required_node) = requirement.get("node").and_then(Value::as_str) else {
        return true;
    };
    let Some(instance_node) = instance.get("node_name").and_then(Value::as_str) else {
        return false;
    };
    instance_node == required_node
        || instance_node.trim_start_matches('/') == required_node.trim_start_matches('/')
}

fn endpoint_name_matches(entity: &Value, name: &str) -> bool {
    let Some(resolved) = entity.get("resolved_name").and_then(Value::as_str) else {
        return false;
    };
    resolved == name || resolved.trim_start_matches('/') == name.trim_start_matches('/')
}

fn entity_type_matches(entity: &Value, interface_type: &str) -> bool {
    let Some(ty) = entity.get("type") else {
        return false;
    };
    match ty {
        Value::String(s) => s == interface_type,
        Value::Object(map) => {
            let package = map.get("package").and_then(Value::as_str).unwrap_or("");
            let name = map.get("name").and_then(Value::as_str).unwrap_or("");
            format!("{package}/{name}") == interface_type
                || format!("{package}::{name}") == interface_type
        }
        _ => false,
    }
}

fn entity_type_display(entity: &Value) -> String {
    match entity.get("type") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(map)) => {
            let package = map.get("package").and_then(Value::as_str).unwrap_or("");
            let name = map.get("name").and_then(Value::as_str).unwrap_or("");
            format!("{package}/{name}")
        }
        _ => "?".to_string(),
    }
}

fn find_source_metadata<'a>(
    metadata: &'a [JsonArtifact],
    package: &str,
    executable: &str,
) -> Option<&'a JsonArtifact> {
    metadata
        .iter()
        .find(|artifact| metadata_matches(&artifact.value, package, executable))
}

fn metadata_matches(value: &Value, package: &str, executable: &str) -> bool {
    let package_match = string_field(value, &["package", "package_name"])
        .is_none_or(|candidate| candidate == package);
    let executable_match = string_field(value, &["executable", "executable_name", "component"])
        .is_none_or(|candidate| candidate == executable);
    package_match && executable_match
}

fn string_field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

fn pairs_field(value: &Value, key: &str) -> Vec<(String, String)> {
    match value.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::Array(pair) if pair.len() == 2 => Some((
                    pair[0].as_str().unwrap_or_default().to_string(),
                    pair[1].as_str().unwrap_or_default().to_string(),
                )),
                Value::Object(map) => {
                    let key = map
                        .get("name")
                        .or_else(|| map.get("from"))
                        .or_else(|| map.get("key"))
                        .and_then(Value::as_str)?;
                    let value = map
                        .get("value")
                        .or_else(|| map.get("to"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Some((key.to_string(), value.to_string()))
                }
                _ => None,
            })
            .collect(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(key, value)| (key.clone(), scalar_to_string(value)))
            .collect(),
        _ => Vec::new(),
    }
}

fn string_list_field(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        other => other.to_string(),
    }
}

fn next_instance_index(
    counts: &mut HashMap<(String, String), usize>,
    package: &str,
    executable: &str,
) -> usize {
    let key = (package.to_string(), executable.to_string());
    let index = *counts.get(&key).unwrap_or(&0);
    counts.insert(key, index + 1);
    index
}

fn artifact_list(artifacts: &[JsonArtifact]) -> String {
    artifacts
        .iter()
        .map(|artifact| artifact.path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn diagnostic(
    severity: &str,
    code: &str,
    message: impl Into<String>,
    package: Option<&str>,
    instance: Option<&str>,
    entity: Option<&str>,
    artifact: &Path,
) -> Value {
    let mut object = Map::new();
    object.insert("severity".to_string(), Value::String(severity.to_string()));
    object.insert("code".to_string(), Value::String(code.to_string()));
    object.insert("message".to_string(), Value::String(message.into()));
    object.insert(
        "source_artifact".to_string(),
        Value::String(artifact.display().to_string()),
    );
    if let Some(package) = package {
        object.insert("package".to_string(), Value::String(package.to_string()));
    }
    if let Some(instance) = instance {
        object.insert("instance".to_string(), Value::String(instance.to_string()));
    }
    if let Some(entity) = entity {
        object.insert("entity".to_string(), Value::String(entity.to_string()));
    }
    Value::Object(object)
}

fn diagnostic_summary(diag: &Value) -> String {
    let code = diag.get("code").and_then(Value::as_str).unwrap_or("error");
    let message = diag.get("message").and_then(Value::as_str).unwrap_or("");
    let artifact = diag
        .get("source_artifact")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut scope = Vec::new();
    for key in ["package", "instance", "entity"] {
        if let Some(value) = diag.get(key).and_then(Value::as_str) {
            scope.push(format!("{key}={value}"));
        }
    }
    if scope.is_empty() {
        format!("{code}: {message} ({artifact})")
    } else {
        format!("{code}: {message} [{}] ({artifact})", scope.join(" "))
    }
}

fn normalize_role(role: &str) -> String {
    match role {
        "pub" | "publisher" => "publisher",
        "sub" | "subscriber" | "subscription" => "subscriber",
        "srv" | "server" | "service_server" => "service_server",
        "cli" | "client" | "service_client" => "service_client",
        "action_server" => "action_server",
        "action_client" => "action_client",
        other => other,
    }
    .to_string()
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_launch_args_in_ros_and_shell_forms() {
        let args = vec!["robot:=alpha".to_string(), "debug=true".to_string()];
        let parsed = parse_launch_args(&args).unwrap();
        assert_eq!(parsed["robot"], "alpha");
        assert_eq!(parsed["debug"], "true");
    }

    #[test]
    fn assigns_distinct_instance_indices() {
        let mut counts = HashMap::new();
        assert_eq!(next_instance_index(&mut counts, "pkg", "talker"), 0);
        assert_eq!(next_instance_index(&mut counts, "pkg", "talker"), 1);
    }

    #[cfg(feature = "play-launch-parser")]
    #[test]
    fn plan_system_parses_launch_and_keeps_distinct_instances() {
        let root = temp_workspace("nros-plan-two-instances");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("package.xml"),
            r#"<package format="3"><name>system_pkg</name><version>0.1.0</version></package>"#,
        )
        .unwrap();
        let launch = root.join("system.launch.xml");
        fs::write(
            &launch,
            r#"<launch>
  <node pkg="demo_pkg" exec="talker" name="talker_a" />
  <node pkg="demo_pkg" exec="talker" name="talker_b" />
</launch>"#,
        )
        .unwrap();
        let metadata = root.join("talker.metadata.json");
        fs::write(
            &metadata,
            r#"{
  "package": "demo_pkg",
  "component": "talker",
  "executable": "talker",
  "nodes": [{
    "id": "node_talker",
    "unresolved_name": {"value": "talker", "kind": "relative"},
    "publishers": [{
      "id": "pub.chatter",
      "unresolved_topic": {"value": "chatter", "kind": "relative"},
      "interface": {"package": "std_msgs", "name": "msg/String", "kind": "message"},
      "qos": null
    }],
    "subscribers": [],
    "timers": [],
    "services": [],
    "actions": []
  }]
}"#,
        )
        .unwrap();

        let output = plan_system(PlanOptions {
            system_pkg: "system_pkg".to_string(),
            workspace_root: root.clone(),
            launch_file: launch,
            record_file: None,
            out_root: root.join("build/system_pkg/nros"),
            metadata_files: vec![metadata],
            manifest_files: vec![],
            nros_toml_files: vec![],
            launch_args: vec![],
        })
        .unwrap();
        let plan: Value =
            serde_json::from_str(&fs::read_to_string(output.plan_path).unwrap()).unwrap();
        serde_json::from_value::<NrosPlan>(plan.clone()).unwrap();
        let instances = plan["instances"].as_array().unwrap();
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[0]["id"], "demo_pkg.talker.0");
        assert_eq!(instances[1]["id"], "demo_pkg.talker.1");
    }

    #[cfg(feature = "play-launch-parser")]
    #[test]
    fn plan_system_resolves_private_remap_and_matches_manifest() {
        let root = temp_workspace("nros-plan-private-remap");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("package.xml"),
            r#"<package format="3"><name>system_pkg</name><version>0.1.0</version></package>"#,
        )
        .unwrap();
        let launch = root.join("system.launch.xml");
        fs::write(
            &launch,
            r#"<launch>
  <node pkg="demo_pkg" exec="driver" name="driver" namespace="/robot">
    <remap from="~/cmd" to="/mux/cmd" />
  </node>
</launch>"#,
        )
        .unwrap();
        let metadata = root.join("driver.metadata.json");
        fs::write(
            &metadata,
            r#"{
  "package": "demo_pkg",
  "component": "driver",
  "executable": "driver",
  "nodes": [{
    "id": "node_driver",
    "unresolved_name": {"value": "driver", "kind": "relative"},
    "publishers": [{
      "id": "pub.cmd",
      "unresolved_topic": {"value": "~/cmd", "kind": "private"},
      "interface": {"package": "std_msgs", "name": "msg/String", "kind": "message"},
      "qos": null
    }],
    "subscribers": [],
    "timers": [{"id": "timer.poll", "period_ms": 100, "callback": "cb.poll"}],
    "services": [],
    "actions": []
  }],
  "callbacks": [{
    "id": "cb.poll",
    "kind": "timer",
    "group": null,
    "effects": [],
    "source": {"artifact": "src/driver.rs", "line": null, "column": null}
  }],
  "parameters": [],
  "trace": {"generator": "test", "package_manifest": "package.xml", "source_artifacts": ["src/driver.rs"]}
}"#,
        )
        .unwrap();
        let manifest = root.join("manifest.launch.yaml");
        fs::write(
            &manifest,
            r#"version: 1
topics:
  /mux/cmd:
    type: std_msgs/msg/String
    pub: [/robot/driver]
"#,
        )
        .unwrap();

        let output = plan_system(PlanOptions {
            system_pkg: "system_pkg".to_string(),
            workspace_root: root.clone(),
            launch_file: launch,
            record_file: None,
            out_root: root.join("build/system_pkg/nros"),
            metadata_files: vec![metadata],
            manifest_files: vec![manifest],
            nros_toml_files: vec![],
            launch_args: vec![],
        })
        .unwrap();
        let plan: Value =
            serde_json::from_str(&fs::read_to_string(output.plan_path).unwrap()).unwrap();
        serde_json::from_value::<NrosPlan>(plan.clone()).unwrap();
        assert_eq!(
            plan["instances"][0]["nodes"][0]["entities"][0]["resolved_name"],
            "/mux/cmd"
        );
        assert_eq!(
            plan["instances"][0]["nodes"][0]["entities"][1]["role"],
            "timer"
        );
        assert!(
            plan["instances"][0]["nodes"][0]["entities"][1]
                .get("resolved_name")
                .is_none()
        );
    }

    #[cfg(feature = "play-launch-parser")]
    #[test]
    fn plan_system_rejects_metadata_entity_missing_from_manifest() {
        let root = temp_workspace("nros-plan-manifest-extra-entity");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("package.xml"),
            r#"<package format="3"><name>system_pkg</name><version>0.1.0</version></package>"#,
        )
        .unwrap();
        let launch = root.join("system.launch.xml");
        fs::write(
            &launch,
            r#"<launch>
  <node pkg="demo_pkg" exec="talker" name="talker" />
</launch>"#,
        )
        .unwrap();
        let metadata = root.join("talker.metadata.json");
        fs::write(
            &metadata,
            r#"{
  "package": "demo_pkg",
  "component": "talker",
  "executable": "talker",
  "nodes": [{
    "id": "node_talker",
    "unresolved_name": {"value": "talker", "kind": "relative"},
    "publishers": [{
      "id": "pub_chatter",
      "unresolved_topic": {"value": "chatter", "kind": "relative"},
      "interface": {"package": "std_msgs", "name": "msg/String", "kind": "message"},
      "qos": null
    }, {
      "id": "pub_extra",
      "unresolved_topic": {"value": "extra", "kind": "relative"},
      "interface": {"package": "std_msgs", "name": "msg/String", "kind": "message"},
      "qos": null
    }],
    "subscribers": [],
    "timers": [],
    "services": [],
    "actions": []
  }],
  "callbacks": [],
  "parameters": [],
  "trace": {"generator": "test", "package_manifest": "package.xml", "source_artifacts": ["src/talker.rs"]}
}"#,
        )
        .unwrap();
        let manifest = root.join("manifest.launch.yaml");
        fs::write(
            &manifest,
            r#"version: 1
topics:
  /chatter:
    type: std_msgs/msg/String
    pub: [/talker]
"#,
        )
        .unwrap();

        let err = plan_system(PlanOptions {
            system_pkg: "system_pkg".to_string(),
            workspace_root: root.clone(),
            launch_file: launch,
            record_file: None,
            out_root: root.join("build/system_pkg/nros"),
            metadata_files: vec![metadata],
            manifest_files: vec![manifest],
            nros_toml_files: vec![],
            launch_args: vec![],
        })
        .unwrap_err()
        .to_string();

        assert!(err.contains("metadata-entity-unmatched"), "{err}");
        assert!(err.contains("/extra"), "{err}");
        assert!(err.contains("pub_extra"), "{err}");
    }

    #[cfg(feature = "play-launch-parser")]
    #[test]
    fn check_plan_rejects_missing_sched_context() {
        let (root, mut plan) = generated_plan("nros-check-missing-sched-context");
        plan["instances"][0]["callbacks"] = serde_json::json!([{
            "id": "demo_pkg.talker.0/cb",
            "source_callback": "cb",
            "group": "default",
            "sched_context": "missing_executor",
            "source": {
                "artifact": "talker.rs",
                "line": null,
                "column": null
            }
        }]);
        let plan_path = root.join("bad-plan.json");
        fs::write(&plan_path, serde_json::to_string_pretty(&plan).unwrap()).unwrap();

        let err = check_plan_file(&plan_path).unwrap_err().to_string();
        assert!(err.contains("missing-sched-context"), "{err}");
    }

    #[cfg(feature = "play-launch-parser")]
    #[test]
    fn check_plan_rejects_unknown_interface_entity() {
        let (root, mut plan) = generated_plan("nros-check-missing-interface-entity");
        plan["interfaces"][0]["used_by"] = serde_json::json!(["missing/entity"]);
        let plan_path = root.join("bad-plan.json");
        fs::write(&plan_path, serde_json::to_string_pretty(&plan).unwrap()).unwrap();

        let err = check_plan_file(&plan_path).unwrap_err().to_string();
        assert!(err.contains("missing-interface-entity"), "{err}");
    }

    #[test]
    fn plan_system_keeps_instance_callbacks_remaps_and_parameter_overrides() {
        let root = temp_workspace("nros-plan-callbacks-params");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("package.xml"),
            r#"<package format="3"><name>system_pkg</name><version>0.1.0</version></package>"#,
        )
        .unwrap();
        let launch = root.join("system.launch.xml");
        fs::write(&launch, "<launch />").unwrap();
        let record = root.join("record.json");
        fs::write(
            &record,
            r#"{
  "node": [
    {
      "package": "demo_pkg",
      "executable": "talker",
      "name": "talker_a",
      "namespace": "/robot_a",
      "remaps": [{"from": "chatter", "to": "/bus/a"}],
      "params": [{"name": "rate_hz", "value": "20"}]
    },
    {
      "package": "demo_pkg",
      "executable": "talker",
      "name": "talker_b",
      "namespace": "/robot_b",
      "remaps": [{"from": "chatter", "to": "/bus/b"}],
      "params": [{"name": "rate_hz", "value": "30"}]
    }
  ]
}"#,
        )
        .unwrap();
        let metadata = root.join("talker.metadata.json");
        fs::write(
            &metadata,
            r#"{
  "version": 1,
  "package": "demo_pkg",
  "component": "talker",
  "language": "rust",
  "executable": "talker",
  "exported_symbol": "nros_component_talker",
  "nodes": [{
    "id": "node_talker",
    "unresolved_name": {"value": "talker", "kind": "relative"},
    "namespace": null,
    "publishers": [{
      "id": "pub_chatter",
      "unresolved_topic": {"value": "chatter", "kind": "relative"},
      "interface": {"package": "std_msgs", "name": "msg/String", "kind": "message"},
      "qos": null
    }],
    "subscribers": [{
      "id": "sub_cmd",
      "unresolved_topic": {"value": "cmd", "kind": "relative"},
      "interface": {"package": "std_msgs", "name": "msg/String", "kind": "message"},
      "qos": null,
      "callback": "cb_cmd"
    }],
    "timers": [],
    "services": [],
    "actions": []
  }],
  "callbacks": [{
    "id": "cb_cmd",
    "kind": "subscription",
    "group": null,
    "effects": [],
    "source": {"artifact": "src/talker.rs", "line": 42, "column": 5}
  }],
  "parameters": [
    {"node": "node_talker", "name": "rate_hz", "default": 10, "read_only": false, "source": {"artifact": "src/talker.rs", "line": 10, "column": 1}},
    {"node": "node_talker", "name": "frame", "default": "map", "read_only": false, "source": {"artifact": "src/talker.rs", "line": 11, "column": 1}}
  ],
  "trace": {"generator": "nros-metadata-rust", "package_manifest": "package.xml", "source_artifacts": ["src/talker.rs"]}
}"#,
        )
        .unwrap();

        let output = plan_system(PlanOptions {
            system_pkg: "system_pkg".to_string(),
            workspace_root: root.clone(),
            launch_file: launch,
            record_file: Some(record),
            out_root: root.join("build/system_pkg/nros"),
            metadata_files: vec![metadata],
            manifest_files: vec![],
            nros_toml_files: vec![],
            launch_args: vec![],
        })
        .unwrap();
        let plan: Value =
            serde_json::from_str(&fs::read_to_string(output.plan_path).unwrap()).unwrap();
        serde_json::from_value::<NrosPlan>(plan.clone()).unwrap();
        let instances = plan["instances"].as_array().unwrap();
        assert_eq!(instances.len(), 2);
        assert_eq!(
            instances[0]["nodes"][0]["entities"][0]["resolved_name"],
            "/bus/a"
        );
        assert_eq!(
            instances[1]["nodes"][0]["entities"][0]["resolved_name"],
            "/bus/b"
        );
        assert_eq!(
            instances[0]["callbacks"][0]["id"],
            "demo_pkg.talker.0/cb_cmd"
        );
        assert_eq!(
            instances[1]["callbacks"][0]["id"],
            "demo_pkg.talker.1/cb_cmd"
        );
        assert_eq!(
            instances[0]["sched_bindings"][0]["callback"],
            "demo_pkg.talker.0/cb_cmd"
        );
        assert_plan_parameter(&instances[0], "rate_hz", json!(20));
        assert_plan_parameter(&instances[1], "rate_hz", json!(30));
        assert_plan_parameter(&instances[0], "frame", json!("map"));
    }

    fn assert_plan_parameter(instance: &Value, name: &str, expected: Value) {
        let parameter = instance["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == name)
            .unwrap_or_else(|| panic!("missing parameter {name}"));
        assert_eq!(parameter["value"], expected);
    }

    #[test]
    fn plan_system_rejects_unknown_callback_effect_entity() {
        let root = temp_workspace("nros-plan-bad-callback-effect");
        let err = plan_with_metadata(
            &root,
            r#"{
  "version": 1,
  "package": "demo_pkg",
  "component": "talker",
  "language": "rust",
  "executable": "talker",
  "exported_symbol": null,
  "nodes": [{
    "id": "node_talker",
    "unresolved_name": {"value": "talker", "kind": "relative"},
    "namespace": null,
    "publishers": [{
      "id": "pub_chatter",
      "unresolved_topic": {"value": "chatter", "kind": "relative"},
      "interface": {"package": "std_msgs", "name": "msg/String", "kind": "message"},
      "qos": null
    }],
    "subscribers": [],
    "timers": [],
    "services": [],
    "actions": []
  }],
  "callbacks": [{
    "id": "cb_timer",
    "kind": "timer",
    "group": null,
    "effects": [{"kind": "publishes", "entity": "missing_pub"}],
    "source": {"artifact": "src/talker.rs", "line": 42, "column": 5}
  }],
  "parameters": [],
  "trace": {"generator": "nros-metadata-rust", "package_manifest": "package.xml", "source_artifacts": ["src/talker.rs"]}
}"#,
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("callback-effect-unknown-entity"), "{err}");
        assert!(err.contains("missing_pub"), "{err}");
    }

    #[test]
    fn plan_system_rejects_missing_entity_callback() {
        let root = temp_workspace("nros-plan-missing-entity-callback");
        let err = plan_with_metadata(
            &root,
            r#"{
  "version": 1,
  "package": "demo_pkg",
  "component": "talker",
  "language": "rust",
  "executable": "talker",
  "exported_symbol": null,
  "nodes": [{
    "id": "node_talker",
    "unresolved_name": {"value": "talker", "kind": "relative"},
    "namespace": null,
    "publishers": [],
    "subscribers": [{
      "id": "sub_cmd",
      "unresolved_topic": {"value": "cmd", "kind": "relative"},
      "interface": {"package": "std_msgs", "name": "msg/String", "kind": "message"},
      "qos": null,
      "callback": "cb_missing"
    }],
    "timers": [],
    "services": [],
    "actions": []
  }],
  "callbacks": [],
  "parameters": [],
  "trace": {"generator": "nros-metadata-rust", "package_manifest": "package.xml", "source_artifacts": ["src/talker.rs"]}
}"#,
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("entity-callback-missing"), "{err}");
        assert!(err.contains("cb_missing"), "{err}");
    }

    #[test]
    fn plan_system_preserves_multiple_source_nodes() {
        let root = temp_workspace("nros-plan-multiple-source-nodes");
        let output = plan_with_metadata(
            &root,
            r#"{
  "version": 1,
  "package": "demo_pkg",
  "component": "talker",
  "language": "rust",
  "executable": "talker",
  "exported_symbol": "nros_component_talker",
  "nodes": [
    {
      "id": "node_talker",
      "unresolved_name": {"value": "talker", "kind": "relative"},
      "namespace": null,
      "publishers": [{
        "id": "pub_chatter",
        "unresolved_topic": {"value": "chatter", "kind": "relative"},
        "interface": {"package": "std_msgs", "name": "msg/String", "kind": "message"},
        "qos": null
      }],
      "subscribers": [],
      "timers": [],
      "services": [],
      "actions": []
    },
    {
      "id": "node_aux",
      "unresolved_name": {"value": "aux", "kind": "relative"},
      "namespace": null,
      "publishers": [],
      "subscribers": [],
      "timers": [],
      "services": [{
        "id": "srv_reset",
        "unresolved_name": {"value": "reset", "kind": "relative"},
        "interface": {"package": "std_srvs", "name": "srv/Trigger", "kind": "service"},
        "callback": "cb_reset"
      }],
      "actions": [{
        "id": "act_nav",
        "unresolved_name": {"value": "navigate", "kind": "relative"},
        "interface": {"package": "nav2_msgs", "name": "action/NavigateToPose", "kind": "action"},
        "goal_callback": "cb_nav_goal",
        "cancel_callback": "cb_nav_cancel",
        "accepted_callback": "cb_nav_accepted"
      }]
    }
  ],
  "callbacks": [
    {"id": "cb_reset", "kind": "service", "group": null, "effects": [{"kind": "sends_service_reply", "entity": "srv_reset"}], "source": {"artifact": "src/lib.rs", "line": 10, "column": 1}},
    {"id": "cb_nav_goal", "kind": "action_goal", "group": null, "effects": [{"kind": "sends_action_goal", "entity": "act_nav"}], "source": {"artifact": "src/lib.rs", "line": 20, "column": 1}},
    {"id": "cb_nav_cancel", "kind": "action_cancel", "group": null, "effects": [], "source": {"artifact": "src/lib.rs", "line": 30, "column": 1}},
    {"id": "cb_nav_accepted", "kind": "action_accepted", "group": null, "effects": [{"kind": "sends_action_result", "entity": "act_nav"}], "source": {"artifact": "src/lib.rs", "line": 40, "column": 1}}
  ],
  "parameters": [],
  "trace": {"generator": "nros-metadata-rust", "package_manifest": "package.xml", "source_artifacts": ["src/lib.rs"]}
}"#,
        )
        .unwrap();
        let plan: Value =
            serde_json::from_str(&fs::read_to_string(output.plan_path).unwrap()).unwrap();
        serde_json::from_value::<NrosPlan>(plan.clone()).unwrap();

        let nodes = plan["instances"][0]["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["source_node"], "node_talker");
        assert_eq!(nodes[0]["resolved_name"], "/talker");
        assert_eq!(
            nodes[0]["entities"][0]["id"],
            "demo_pkg.talker.0/pub_chatter"
        );
        assert_eq!(nodes[1]["source_node"], "node_aux");
        assert_eq!(nodes[1]["resolved_name"], "/aux");
        assert_eq!(nodes[1]["entities"][0]["role"], "service_server");
        assert_eq!(nodes[1]["entities"][1]["role"], "action_server");
    }

    #[test]
    fn plan_system_rejects_rust_component_missing_exported_symbol() {
        let root = temp_workspace("nros-plan-missing-export");
        let err = plan_with_metadata(
            &root,
            r#"{
  "version": 1,
  "package": "demo_pkg",
  "component": "talker",
  "language": "rust",
  "executable": "talker",
  "exported_symbol": null,
  "nodes": [{
    "id": "node_talker",
    "unresolved_name": {"value": "talker", "kind": "relative"},
    "namespace": null,
    "publishers": [],
    "subscribers": [],
    "timers": [],
    "services": [],
    "actions": []
  }],
  "callbacks": [],
  "parameters": [],
  "trace": {"generator": "nros-metadata-rust", "package_manifest": "package.xml", "source_artifacts": ["src/talker.rs"]}
}"#,
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("missing-component-export"), "{err}");
        assert!(err.contains("component_export"), "{err}");
    }

    fn plan_with_metadata(root: &Path, metadata_json: &str) -> Result<PlanningOutput> {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("package.xml"),
            r#"<package format="3"><name>system_pkg</name><version>0.1.0</version></package>"#,
        )
        .unwrap();
        let launch = root.join("system.launch.xml");
        fs::write(&launch, "<launch />").unwrap();
        let record = root.join("record.json");
        fs::write(
            &record,
            r#"{"node":[{"package":"demo_pkg","executable":"talker","name":"talker"}]}"#,
        )
        .unwrap();
        let metadata = root.join("talker.metadata.json");
        fs::write(&metadata, metadata_json).unwrap();

        plan_system(PlanOptions {
            system_pkg: "system_pkg".to_string(),
            workspace_root: root.to_path_buf(),
            launch_file: launch,
            record_file: Some(record),
            out_root: root.join("build/system_pkg/nros"),
            metadata_files: vec![metadata],
            manifest_files: vec![],
            nros_toml_files: vec![],
            launch_args: vec![],
        })
    }

    #[cfg(feature = "play-launch-parser")]
    fn generated_plan(name: &str) -> (PathBuf, Value) {
        let root = temp_workspace(name);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("package.xml"),
            r#"<package format="3"><name>system_pkg</name><version>0.1.0</version></package>"#,
        )
        .unwrap();
        let launch = root.join("system.launch.xml");
        fs::write(
            &launch,
            r#"<launch>
  <node pkg="demo_pkg" exec="talker" name="talker" />
</launch>"#,
        )
        .unwrap();
        let metadata = root.join("talker.metadata.json");
        fs::write(
            &metadata,
            r#"{
  "package": "demo_pkg",
  "component": "talker",
  "executable": "talker",
  "nodes": [{
    "id": "node_talker",
    "unresolved_name": {"value": "talker", "kind": "relative"},
    "publishers": [{
      "id": "pub.chatter",
      "unresolved_topic": {"value": "chatter", "kind": "relative"},
      "interface": {"package": "std_msgs", "name": "msg/String", "kind": "message"},
      "qos": null
    }],
    "subscribers": [],
    "timers": [],
    "services": [],
    "actions": []
  }]
}"#,
        )
        .unwrap();

        let output = plan_system(PlanOptions {
            system_pkg: "system_pkg".to_string(),
            workspace_root: root.clone(),
            launch_file: launch,
            record_file: None,
            out_root: root.join("build/system_pkg/nros"),
            metadata_files: vec![metadata],
            manifest_files: vec![],
            nros_toml_files: vec![],
            launch_args: vec![],
        })
        .unwrap();
        let plan = serde_json::from_str(&fs::read_to_string(output.plan_path).unwrap()).unwrap();
        (root, plan)
    }

    fn temp_workspace(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{}-{stamp}", std::process::id()))
    }
}
