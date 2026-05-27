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
    plan::{
        LifecycleAutostart, PlanBuildOptions, PlanEntity, PlanInstance, PlanSchedContext,
        TransportKind,
    },
    schema::{DeadlinePolicy, ParameterValue, SchedClass},
};

const CARGO_TEMPLATE: &str = include_str!("../../templates/orchestration/Cargo.toml.jinja");
const BUILD_TEMPLATE: &str = include_str!("../../templates/orchestration/build.rs.jinja");
const LIB_TEMPLATE: &str = include_str!("../../templates/orchestration/lib.rs.jinja");
const ZEPHYR_CMAKE_TEMPLATE: &str =
    include_str!("../../templates/orchestration/zephyr/CMakeLists.txt.jinja");
const ZEPHYR_PRJ_CONF_TEMPLATE: &str =
    include_str!("../../templates/orchestration/zephyr/prj.conf.jinja");

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
    let cargo_config = render_cargo_config(options, &plan);
    let rust_toolchain = render_rust_toolchain(&plan);

    write_if_changed(&options.output_dir.join("Cargo.toml"), &cargo_toml)?;
    write_if_changed(&options.output_dir.join("build.rs"), &build_rs)?;
    // Zephyr ships a Rust staticlib (`name = "rustapp"`,
    // `crate-type = ["staticlib"]`) consumed by zephyr-lang-rust's
    // `rust_cargo_application()` CMake function — the cargo source
    // file is `src/lib.rs`, not `src/main.rs`. Every other platform
    // uses a binary crate with `src/main.rs`.
    if matches!(
        profile(&plan.build.board, &plan.build.target).map(|p| p.entry_kind),
        Some(EntryKind::ZephyrStaticlib)
    ) {
        write_if_changed(&src_dir.join("lib.rs"), LIB_TEMPLATE)?;
        let cmake = render_zephyr_cmake(options);
        let prj_conf = render_zephyr_prj_conf(&plan);
        write_if_changed(&options.output_dir.join("CMakeLists.txt"), &cmake)?;
        write_if_changed(&options.output_dir.join("prj.conf"), &prj_conf)?;
    } else if emits_entry_lib(&plan) {
        // Phase 172 entry lib: the wiring + Rust API live in `src/lib.rs`;
        // `src/main.rs` is a thin `self` shim (hosted `fn main`, or a no_std
        // board shim driven by the board rlib's `run()`).
        write_if_changed(&src_dir.join("lib.rs"), &render_entry_lib_rs(&plan))?;
        let board_shim = matches!(
            profile(&plan.build.board, &plan.build.target).map(|p| p.entry_kind),
            Some(EntryKind::BoardRun)
        );
        let shim = if board_shim {
            render_board_shim_main(options, &plan)
        } else {
            render_hosted_shim_main(options, &plan)
        };
        write_if_changed(&src_dir.join("main.rs"), &shim)?;
        // The C ABI + its cbindgen header + the vendor-includable CMake
        // fragment ship only with the std-hosted (alloc) entry lib; a board
        // `self` shim calls the Rust API directly and needs none of it.
        if uses_std(&plan.build) {
            let include_dir = options.output_dir.join("include");
            fs::create_dir_all(&include_dir).wrap_err_with(|| {
                format!(
                    "failed to create generated package include dir {}",
                    include_dir.display()
                )
            })?;
            write_if_changed(
                &include_dir.join(format!("{}.h", system_ident(&plan))),
                &render_entry_header(&plan),
            )?;
            write_if_changed(
                &options.output_dir.join("CMakeLists.txt"),
                &render_entry_cmake(options, &plan),
            )?;
        }
    } else {
        write_if_changed(&src_dir.join("main.rs"), &render_main(&plan))?;
    }
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
    if let Some(toolchain) = rust_toolchain {
        write_if_changed(&options.output_dir.join("rust-toolchain.toml"), &toolchain)?;
    }
    // Phase 173.7 — NuttX is RtosOwned: the NuttX kernel owns the net
    // stack, so transport IP lands in the NuttX defconfig, not the board
    // `Config`. Emit an additive `nuttx-net.defconfig` fragment from
    // `[[transport]]` for the user to merge into the board defconfig
    // (NuttX is built out-of-tree). No transports ⇒ no file.
    if let Some(fragment) = nuttx_net_fragment(&plan) {
        write_if_changed(&options.output_dir.join("nuttx-net.defconfig"), &fragment)?;
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
            "{{ lib_section }}",
            &render_lib_section(plan, &options.package_name),
        )
        .replace(
            "{{ default_features }}",
            &toml_string_array(&generated_default_features(
                &plan.build,
                plan.lifecycle.is_some(),
                plan.param_persistence.is_some(),
            )),
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
        .replace("{{ build_dependencies }}", &render_build_dependencies(plan))
}

/// Phase 126.M5.zephyr — zephyr-lang-rust's
/// `rust_cargo_application()` looks for a staticlib named
/// `rustapp` (its CMakeLists.txt hard-codes the link line against
/// `libstaticlib.a → librustapp.a`). Every other platform stays a
/// regular binary crate.
fn render_lib_section(plan: &NrosPlan, package_name: &str) -> String {
    if matches!(
        profile(&plan.build.board, &plan.build.target).map(|p| p.entry_kind),
        Some(EntryKind::ZephyrStaticlib)
    ) {
        return "\n[lib]\nname = \"rustapp\"\ncrate-type = [\"staticlib\"]\n".to_string();
    }
    // Phase 172 entry lib: a `lib` for the self shim bin to call. The hosted
    // (alloc) lib also emits a `staticlib` for vendor linking; a board `self`
    // lib stays `lib`-only — a no_std `staticlib` is a final artifact that
    // would need its own panic handler (which lives in the bin shim).
    if emits_entry_lib(plan) {
        let crate_types = if uses_std(&plan.build) {
            "[\"lib\", \"staticlib\"]"
        } else {
            "[\"lib\"]"
        };
        return format!(
            "\n[lib]\nname = \"{}\"\ncrate-type = {crate_types}\n",
            crate_ident(package_name)
        );
    }
    String::new()
}

/// Phase 126.M5.zephyr — zephyr-lang-rust requires `zephyr-build`
/// in `[build-dependencies]` so Kconfig constants reach the Rust
/// staticlib at compile time. Other platforms have an empty
/// build-deps section today.
fn render_build_dependencies(plan: &NrosPlan) -> String {
    match profile(&plan.build.board, &plan.build.target).map(|p| p.entry_kind) {
        Some(EntryKind::ZephyrStaticlib) => "zephyr-build = \"0.1.0\"\n".to_string(),
        _ => String::new(),
    }
}

/// Phase 173.2b — crate-root preamble shared by every generated
/// `src/main.rs`: the `nros_generated` include module plus the two
/// always-present `use`s. Board-specific crate-root items
/// (`crate_root_extra`) and the no_std/no_main attrs are layered on by
/// `render_main`.
const MAIN_PREAMBLE: &str = "\
mod nros_generated {
    core::include!(core::concat!(core::env!(\"OUT_DIR\"), \"/nros_generated.rs\"));
}

use nros::prelude::*;";

/// Phase 173.2b — the `run_system` helper, emitted verbatim into every
/// non-Zephyr `src/main.rs` (formerly `main.rs.jinja` lines 49-78). It is
/// platform-agnostic: each entry shape just builds an `ExecutorConfig`
/// and hands it to `run_system`.
const RUN_SYSTEM: &str = "\
fn run_system(config: ExecutorConfig<'_>) -> core::result::Result<(), nros::NodeError> {
    run_executor(nros_generated::build_executor(&config)?)
}

fn run_executor(mut executor: Executor) -> core::result::Result<(), nros::NodeError> {
    // Phase 172 WP-B — registration moved into the entry lib
    // (`nros_generated::register_all`), the unit the entry-lib C ABI wraps;
    // the per-platform entry now only opens + spins.
    nros_generated::register_all(&mut executor)?;

    #[cfg(feature = \"std\")]
    return executor.spin_blocking(SpinOptions::default());

    #[cfg(not(feature = \"std\"))]
    executor.spin_default()
}";

/// Phase 173.5 — bridge entry helper, emitted only in bridge mode. Opens
/// every declared transport's RMW session via `Executor::open_multi`,
/// then runs the same post-open flow as `run_system`.
const RUN_SYSTEM_BRIDGE: &str = "\
fn run_system_bridge() -> core::result::Result<(), nros::NodeError> {
    run_executor(nros_generated::build_executor_bridge()?)
}";

/// Phase 173.2b — render the generated `src/main.rs` from the resolved
/// `profile()`, replacing the static `main.rs.jinja` (which shipped every
/// platform's `#[cfg(feature = \"platform-X\")]` entry block). One entry
/// shape is chosen by the profile's `board_entry`:
///
/// * `None` — hosted native/posix `fn main` that builds
///   `ExecutorConfig::from_env()` (std) / `default_const()` (no_std) and
///   calls `run_system`.
/// * `Some(entry)` — `<board>::run(<board>::Config::default(), closure)`
///   where the closure threads the board `Config` into `ExecutorConfig`.
///   no_std/no_main is emitted when the entry is a bare-metal `BoardRun`
///   (threadx-linux is a `HostedMain` board host and stays std).
///
/// The `nros-orchestration` import is unused on Zephyr's staticlib path,
/// so this is only invoked for the binary-crate (`main.rs`) platforms.
/// Parse `"10.0.2.50/24"` (or bare `"10.0.2.50"`, prefix defaults 24)
/// into octets + prefix. `None` on malformed input.
fn parse_ipv4_cidr(s: &str) -> Option<([u8; 4], u8)> {
    let (addr, prefix) = s.split_once('/').unwrap_or((s, "24"));
    let prefix: u8 = prefix.parse().ok()?;
    let mut octets = [0u8; 4];
    let mut n = 0;
    for part in addr.split('.') {
        if n == 4 {
            return None;
        }
        octets[n] = part.parse().ok()?;
        n += 1;
    }
    (n == 4).then_some((octets, prefix))
}

/// Parse `"02:00:00:00:00:01"` (colon- or dash-separated hex) into 6
/// octets. `None` on malformed input.
fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut octets = [0u8; 6];
    let mut n = 0;
    for part in s.split([':', '-']) {
        if n == 6 {
            return None;
        }
        octets[n] = u8::from_str_radix(part, 16).ok()?;
        n += 1;
    }
    (n == 6).then_some(octets)
}

/// Phase 173.5 / 172.J — the `BoardTransportConfig` setter calls the
/// generated `apply_transport_config` emits, derived from `[[transport]]`:
/// a static ethernet `ip` → `set_ipv4`, `mac` → `set_mac`, `gateway` →
/// `set_gateway`, a serial `baudrate` → `set_baudrate`. `dhcp` and
/// missing/malformed values emit nothing.
fn transport_config_setter_calls(build: &PlanBuildOptions) -> Vec<String> {
    let mut calls = Vec::new();
    for t in &build.transports {
        if let Some(ip) = t.ip.as_deref()
            && !ip.eq_ignore_ascii_case("dhcp")
            && let Some((o, prefix)) = parse_ipv4_cidr(ip)
        {
            calls.push(format!(
                "    c.set_ipv4([{}, {}, {}, {}], {prefix});",
                o[0], o[1], o[2], o[3]
            ));
        }
        if let Some(mac) = t.mac.as_deref().and_then(parse_mac) {
            calls.push(format!(
                "    c.set_mac([0x{:02x}, 0x{:02x}, 0x{:02x}, 0x{:02x}, 0x{:02x}, 0x{:02x}]);",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            ));
        }
        if let Some((o, _)) = t.gateway.as_deref().and_then(parse_ipv4_cidr) {
            calls.push(format!(
                "    c.set_gateway([{}, {}, {}, {}]);",
                o[0], o[1], o[2], o[3]
            ));
        }
        if let Some(baud) = t.baudrate {
            calls.push(format!("    c.set_baudrate({baud});"));
        }
        // Phase 172.K.4 — wifi credentials. `{:?}` quotes + escapes the string
        // literal for the generated Rust.
        if let Some(ssid) = t.ssid.as_deref() {
            calls.push(format!("    c.set_ssid({ssid:?});"));
        }
        if let Some(password) = t.password.as_deref() {
            calls.push(format!("    c.set_password({password:?});"));
        }
    }
    calls
}

/// Whether the generator emits an `apply_transport_config` fn + the
/// board-entry mutation: only for `NanoRosOwned` board platforms (the
/// board owns the net stack) that declare a static IP or baud. RtosOwned
/// targets route IP into a config fragment instead (Phase 173.7); hosted
/// (posix) has no board `Config`.
fn emits_transport_config_override(plan: &NrosPlan) -> bool {
    let Some(p) = profile(&plan.build.board, &plan.build.target) else {
        return false;
    };
    p.board_entry.is_some()
        && p.net_stack == NetStack::NanoRosOwned
        && !transport_config_setter_calls(&plan.build).is_empty()
}

fn render_main(plan: &NrosPlan) -> String {
    let profile = profile(&plan.build.board, &plan.build.target);
    let board_entry = profile.and_then(|p| p.board_entry);
    let no_std = matches!(
        profile,
        Some(PlatformProfile {
            entry_kind: EntryKind::BoardRun,
            board_entry: Some(_),
            ..
        })
    );

    let mut out = String::new();
    if no_std {
        out.push_str("#![no_std]\n#![no_main]\n\n");
    }
    out.push_str(MAIN_PREAMBLE);
    out.push('\n');

    if let Some(entry) = board_entry
        && !entry.crate_root_extra.is_empty()
    {
        out.push('\n');
        out.push_str(entry.crate_root_extra);
        out.push('\n');
    }

    out.push('\n');
    out.push_str(RUN_SYSTEM);
    out.push('\n');

    let bridge = plan.build.is_bridge();
    if bridge {
        out.push('\n');
        out.push_str(RUN_SYSTEM_BRIDGE);
        out.push('\n');
    }

    let apply_config = emits_transport_config_override(plan);

    out.push('\n');
    match board_entry {
        None => out.push_str(if bridge {
            HOSTED_MAIN_BRIDGE
        } else {
            HOSTED_MAIN
        }),
        Some(entry) => out.push_str(&render_board_entry(&entry, bridge, apply_config)),
    }
    out.push('\n');

    out
}

/// Hosted native/posix entry (formerly `main.rs.jinja` lines 88-95).
const HOSTED_MAIN: &str = "\
fn main() -> core::result::Result<(), nros::NodeError> {
    #[cfg(feature = \"std\")]
    let config = ExecutorConfig::from_env().node_name(nros_generated::SYSTEM.default_node_name());
    #[cfg(not(feature = \"std\"))]
    let config =
        ExecutorConfig::default_const().node_name(nros_generated::SYSTEM.default_node_name());
    run_system(config)
}";

/// Phase 173.5 — hosted bridge entry: the per-transport sessions come
/// from `SESSION_SPECS`, so the `ExecutorConfig` is bypassed entirely.
const HOSTED_MAIN_BRIDGE: &str = "\
fn main() -> core::result::Result<(), nros::NodeError> {
    run_system_bridge()
}";

/// Phase 172 WP-B — the generated package's Rust crate identifier (its `[lib]`
/// name): the package name with every non-alphanumeric char folded to `_`.
fn crate_ident(package_name: &str) -> String {
    package_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Phase 172 WP-B — the system identifier in the entry-lib C ABI symbol prefix
/// + header name: `plan.system` lowercased, non-alphanumeric → `_`.
fn system_ident(plan: &NrosPlan) -> String {
    plan.system
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Phase 172 WP-B — emit the two-form entry lib (compiled form) instead of a
/// bare `main`: a `src/lib.rs` exposing the Rust-native API + the C ABI
/// (`crate-type = ["lib", "staticlib"]`) plus a thin `src/main.rs` self shim.
///
/// Scoped this slice to **std-hosted, non-bridge `self`** targets
/// (native/posix/linux) — the form the `orchestration_e2e` fixture builds +
/// boots. Board, Zephyr, no_std-hosted, and bridge entries keep their current
/// emitter until later WP-B slices generalize the lib + add the source form.
fn emits_entry_lib(plan: &NrosPlan) -> bool {
    if plan.build.is_bridge() {
        return false;
    }
    match profile(&plan.build.board, &plan.build.target) {
        // Hosted `self`: the std entry lib + a hosted shim.
        Some(p) if p.entry_kind == EntryKind::HostedMain => uses_std(&plan.build),
        // Board `self`: a no_std entry lib + a board shim (driven by the
        // board rlib's `run()`). Requires a `board_entry` — NuttX/orin-spe
        // (BoardRun, no board_entry) keep the legacy hosted-`main` path until
        // they get one.
        Some(p) if p.entry_kind == EntryKind::BoardRun => p.board_entry.is_some(),
        _ => false,
    }
}

/// Phase 172 WP-B — `src/lib.rs` for the compiled-form entry lib: hosts the
/// generated wiring tables, re-exports the Rust-native API, and defines the
/// `nros_<sys>_*` C ABI over an opaque heap-owned `Executor` handle.
fn render_entry_lib_rs(plan: &NrosPlan) -> String {
    let sys = system_ident(plan);
    // Board targets compile the entry lib `#![no_std]`. The C ABI (a
    // heap-owned executor handle) needs an allocator + std-style boxing, so
    // it rides the std-hosted path; a board `self` shim calls the Rust API
    // (`register_all`) directly and doesn't need it.
    let no_std = !uses_std(&plan.build);
    let c_abi = uses_std(&plan.build);

    let mut out = String::new();
    if no_std {
        out.push_str("#![no_std]\n\n");
    }
    out.push_str("//! Generated nano-ros entry library (Phase 172 WP-B, compiled form).\n");
    out.push_str("//!\n//! Hosts the system wiring tables + the Rust-native entry API\n");
    out.push_str("//! (`build_executor` / `register_all`).\n\n");
    if c_abi {
        // The heap-owned C-ABI handle boxes through `alloc` (not `std`).
        out.push_str("extern crate alloc;\n\n");
    }
    out.push_str("mod nros_generated {\n");
    out.push_str(
        "    core::include!(core::concat!(core::env!(\"OUT_DIR\"), \"/nros_generated.rs\"));\n",
    );
    out.push_str("}\n\n");
    // Re-export the wiring the board `self` shim needs (`TRANSPORT_LOCATOR`
    // for the baked locator; `apply_transport_config` when the board owns the
    // net stack) alongside the core API.
    let reexports = if emits_transport_config_override(plan) {
        "SYSTEM, TRANSPORT_LOCATOR, apply_transport_config, build_executor, register_all"
    } else {
        "SYSTEM, TRANSPORT_LOCATOR, build_executor, register_all"
    };
    out.push_str(&format!("pub use nros_generated::{{{reexports}}};\n\n"));

    if !c_abi {
        // Board self: Rust API only; the shim calls `build_executor` +
        // `register_all`.
        return out;
    }

    out.push_str("// --- Entry-lib C ABI (Phase 172 WP-B) ---\n\n");
    // Phase 172 WP-B — config lowering: the optional runtime `Config` override.
    // Precedence is param > env > baked — a non-NULL `NrosConfig` overrides the
    // env/baked defaults `build_executor` would otherwise use.
    out.push_str(
        "/// Optional runtime config override passed to `build_executor`\n\
         /// (precedence: param > env > baked). Unset fields fall through.\n\
         #[repr(C)]\n\
         pub struct NrosConfig {\n\
         \x20   /// ROS 2 domain ID; negative ⇒ unset (env/baked).\n\
         \x20   pub domain_id: i32,\n\
         \x20   /// Middleware locator (NUL-terminated); NULL ⇒ unset.\n\
         \x20   pub locator: *const core::ffi::c_char,\n\
         }\n\n",
    );
    out.push_str(&format!(
        "/// Build the system executor. `cfg` overrides env/baked config\n\
         /// (precedence param > env > baked); NULL ⇒ env/baked. Heap-owned — free\n\
         /// with `nros_{sys}_destroy`; returns NULL on error.\n\
         #[unsafe(no_mangle)]\n\
         pub extern \"C\" fn nros_{sys}_build_executor(cfg: *const NrosConfig) -> *mut nros::Executor {{\n\
         \x20   let mut config: nros::ExecutorConfig<'_> =\n\
         \x20       nros::ExecutorConfig::from_env().node_name(nros_generated::SYSTEM.default_node_name());\n\
         \x20   // Apply the param override (highest precedence). The locator borrow\n\
         \x20   // lives only across the immediate `build_executor` open below.\n\
         \x20   let locator_override: Option<&str> = match unsafe {{ cfg.as_ref() }} {{\n\
         \x20       Some(cfg) => {{\n\
         \x20           if cfg.domain_id >= 0 {{ config = config.domain_id(cfg.domain_id as u32); }}\n\
         \x20           if cfg.locator.is_null() {{ None }} else {{ unsafe {{ core::ffi::CStr::from_ptr(cfg.locator) }}.to_str().ok() }}\n\
         \x20       }}\n\
         \x20       None => None,\n\
         \x20   }};\n\
         \x20   if let Some(locator) = locator_override {{ config.locator = locator; }}\n\
         \x20   match nros_generated::build_executor(&config) {{\n\
         \x20       Ok(executor) => alloc::boxed::Box::into_raw(alloc::boxed::Box::new(executor)),\n\
         \x20       Err(_) => core::ptr::null_mut(),\n\
         \x20   }}\n\
         }}\n\n"
    ));
    out.push_str(&format!(
        "/// Register sched contexts + every node + lifecycle + param persistence.\n\
         #[unsafe(no_mangle)]\n\
         pub extern \"C\" fn nros_{sys}_register_all(executor: *mut nros::Executor) -> i32 {{\n\
         \x20   match unsafe {{ executor.as_mut() }} {{\n\
         \x20       Some(executor) => match nros_generated::register_all(executor) {{ Ok(()) => 0, Err(_) => -1 }},\n\
         \x20       None => -1,\n\
         \x20   }}\n\
         }}\n\n"
    ));
    out.push_str(&format!(
        "/// Spin the executor (blocking) until shutdown.\n\
         #[unsafe(no_mangle)]\n\
         pub extern \"C\" fn nros_{sys}_spin(executor: *mut nros::Executor) -> i32 {{\n\
         \x20   match unsafe {{ executor.as_mut() }} {{\n\
         \x20       Some(executor) => match executor.spin_blocking(nros::SpinOptions::default()) {{ Ok(()) => 0, Err(_) => -1 }},\n\
         \x20       None => -1,\n\
         \x20   }}\n\
         }}\n\n"
    ));
    out.push_str(&format!(
        "/// Free an executor returned by `nros_{sys}_build_executor`.\n\
         #[unsafe(no_mangle)]\n\
         pub extern \"C\" fn nros_{sys}_destroy(executor: *mut nros::Executor) {{\n\
         \x20   if !executor.is_null() {{\n\
         \x20       drop(unsafe {{ alloc::boxed::Box::from_raw(executor) }});\n\
         \x20   }}\n\
         }}\n"
    ));
    out
}

/// Phase 172 WP-B — the thin `self` startup shim `src/main.rs`: opens +
/// registers the system through the entry lib, then spins. All wiring lives in
/// `lib.rs`; the shim only exists so a `self` deploy produces a runnable binary.
fn render_hosted_shim_main(options: &GenerateOptions, _plan: &NrosPlan) -> String {
    let krate = crate_ident(&options.package_name);
    format!(
        "//! Generated `self` startup shim (Phase 172 WP-B). All wiring lives in the\n\
         //! entry lib (`lib.rs`); this only opens + registers + spins.\n\n\
         use nros::prelude::*;\n\
         use {krate}::{{SYSTEM, build_executor, register_all}};\n\n\
         fn main() -> core::result::Result<(), nros::NodeError> {{\n\
         \x20   let config = ExecutorConfig::from_env().node_name(SYSTEM.default_node_name());\n\
         \x20   let mut executor = build_executor(&config)?;\n\
         \x20   register_all(&mut executor)?;\n\
         \x20   executor.spin_blocking(SpinOptions::default())\n\
         }}\n"
    )
}

/// Generated board `self` startup shim: a `#![no_std]` `main` driven by the
/// board rlib's `run()` (hardware + transport bring-up), whose closure builds,
/// registers, and spins the system via the entry lib's Rust API. Mirrors the
/// hosted shim on the board entry pattern, replacing the legacy inlined
/// `run_system`. The board self shim needs no C ABI.
fn render_board_shim_main(options: &GenerateOptions, plan: &NrosPlan) -> String {
    let krate = crate_ident(&options.package_name);
    let entry = profile(&plan.build.board, &plan.build.target)
        .and_then(|p| p.board_entry)
        .expect("render_board_shim_main: board_entry present (gated by emits_entry_lib)");
    let apply_config = emits_transport_config_override(plan);
    let cfg_expr = if apply_config {
        format!(
            "{{\n            let mut cfg = {b}::Config::default();\n\
             \x20           {krate}::apply_transport_config(&mut cfg);\n\
             \x20           cfg\n        }}",
            b = entry.crate_name,
        )
    } else {
        format!("{b}::Config::default()", b = entry.crate_name)
    };

    let mut out = String::new();
    out.push_str("#![no_std]\n#![no_main]\n\n");
    out.push_str(
        "//! Generated board `self` startup shim (Phase 172 entry-lib). The board\n\
         //! rlib's `run()` boots hardware, then the closure builds + registers +\n\
         //! spins via the entry lib (`lib.rs`).\n\n",
    );
    out.push_str("use nros::prelude::*;\n");
    out.push_str(&format!(
        "use {krate}::{{SYSTEM, TRANSPORT_LOCATOR, build_executor, register_all}};\n"
    ));
    if !entry.crate_root_extra.is_empty() {
        out.push_str(entry.crate_root_extra);
        out.push('\n');
    }
    out.push('\n');
    if !entry.comment.is_empty() {
        out.push_str(entry.comment);
        out.push('\n');
    }
    out.push_str(entry.signature);
    out.push_str(&format!(
        " {{\n    {b}::run(\n        {cfg_expr},\n        |board_config| -> core::result::Result<(), nros::NodeError> {{\n\
         \x20           let config = ExecutorConfig::new(TRANSPORT_LOCATOR.unwrap_or(board_config.zenoh_locator))\n\
         \x20               .domain_id(board_config.domain_id)\n\
         \x20               .node_name(SYSTEM.default_node_name()){extra};\n\
         \x20           let mut executor = build_executor(&config)?;\n\
         \x20           register_all(&mut executor)?;\n\
         \x20           executor.spin_default()\n\
         \x20       }},\n    )\n}}\n",
        b = entry.crate_name,
        extra = entry.closure_extra,
    ));
    out
}

/// Phase 172 WP-B — the cbindgen-shaped C header for the compiled-form entry
/// lib. Emitted directly (the ABI is fixed + known at generation time, so no
/// build-time cbindgen scan is needed); names the opaque handle `NrosExecutor`
/// to match `nros-c`.
fn render_entry_header(plan: &NrosPlan) -> String {
    let sys = system_ident(plan);
    let guard = format!("NROS_ENTRY_{}_H", sys.to_uppercase());
    format!(
        "/* Generated nano-ros entry-lib C ABI (Phase 172 WP-B). Do not edit. */\n\
         #ifndef {guard}\n#define {guard}\n\n#include <stdint.h>\n\n\
         #ifdef __cplusplus\nextern \"C\" {{\n#endif\n\n\
         /* Opaque executor handle (as in nros-c). */\n\
         typedef struct NrosExecutor NrosExecutor;\n\n\
         /* Optional runtime config override (precedence: param > env > baked). */\n\
         typedef struct NrosConfig {{\n\
         \x20   int32_t domain_id;     /* < 0 => unset (env/baked) */\n\
         \x20   const char *locator;   /* NULL => unset */\n\
         }} NrosConfig;\n\n\
         NrosExecutor *nros_{sys}_build_executor(const NrosConfig *cfg);\n\
         int32_t nros_{sys}_register_all(NrosExecutor *executor);\n\
         int32_t nros_{sys}_spin(NrosExecutor *executor);\n\
         void nros_{sys}_destroy(NrosExecutor *executor);\n\n\
         #ifdef __cplusplus\n}}\n#endif\n\n#endif /* {guard} */\n"
    )
}

/// Phase 172 WP-B — the entry lib's **source-form** CMake fragment. A
/// vendor-owns-toolchain deploy (`emit = "source"`) `add_subdirectory()`s the
/// generated package; Corrosion (loaded by the vendor project) compiles the
/// crate's `staticlib` in the vendor's toolchain, and the fragment exposes it
/// as `<sys>_entry` with the C ABI header on the include path. Emitted
/// alongside the compiled artifacts so one generated package serves both forms;
/// the `nros deploy` runner picks per `[deploy].emit`.
fn render_entry_cmake(options: &GenerateOptions, plan: &NrosPlan) -> String {
    let sys = system_ident(plan);
    let krate = crate_ident(&options.package_name);
    format!(
        "# Generated nano-ros entry lib — source form (Phase 172 WP-B). Do not edit.\n\
         #\n\
         # A vendor CMake project that has Corrosion loaded consumes this with:\n\
         #   add_subdirectory(<this_dir> {sys}_entry)\n\
         #   target_link_libraries(<app> PRIVATE {sys}_entry)\n\
         # Corrosion compiles the generated crate's staticlib in the vendor\n\
         # toolchain; the `nros_{sys}_*` C ABI header is on the include path.\n\
         cmake_minimum_required(VERSION 3.22)\n\n\
         if(NOT COMMAND corrosion_import_crate)\n\
         \x20   message(FATAL_ERROR\n\
         \x20       \"nano-ros entry lib (source form) needs Corrosion — load it before add_subdirectory()\")\n\
         endif()\n\n\
         corrosion_import_crate(\n\
         \x20   MANIFEST_PATH \"${{CMAKE_CURRENT_LIST_DIR}}/Cargo.toml\"\n\
         \x20   CRATES {krate}\n\
         \x20   CRATE_TYPES staticlib)\n\n\
         add_library({sys}_entry INTERFACE)\n\
         target_link_libraries({sys}_entry INTERFACE {krate})\n\
         target_include_directories({sys}_entry INTERFACE \"${{CMAKE_CURRENT_LIST_DIR}}/include\")\n"
    )
}

/// Render the `<board>::run(..)` entry for a board-driven platform. The
/// `ExecutorConfig` builder chain is identical across boards apart from
/// the board crate name and the per-board `closure_extra` suffix.
fn render_board_entry(entry: &BoardEntry, bridge: bool, apply_config: bool) -> String {
    let mut out = String::new();
    if !entry.comment.is_empty() {
        out.push_str(entry.comment);
        out.push('\n');
    }
    out.push_str(entry.signature);
    out.push_str(" {\n");

    // The board `Config` handed to `run`: either a plain default, or a
    // default with the nros.toml transport IP / baud applied (NanoRosOwned
    // — Phase 173.5). Either way `run` drives hardware init from it.
    let cfg_expr = if apply_config {
        format!(
            "{{\n            let mut cfg = {crate}::Config::default();\n            nros_generated::apply_transport_config(&mut cfg);\n            cfg\n        }}",
            crate = entry.crate_name,
        )
    } else {
        format!("{crate}::Config::default()", crate = entry.crate_name)
    };

    if bridge {
        // Bridge mode: the board `run` still drives hardware init, but the
        // sessions come from `SESSION_SPECS` via `run_system_bridge` — the
        // single-session `ExecutorConfig` is unused.
        out.push_str(&format!(
            "    {crate}::run(\n        {cfg_expr},\n        |_board_config| {{\n            run_system_bridge()\n        }},\n    )\n}}",
            crate = entry.crate_name,
        ));
    } else {
        out.push_str(&format!(
            "    {crate}::run(\n        {cfg_expr},\n        |board_config| {{\n            let config = ExecutorConfig::new(nros_generated::TRANSPORT_LOCATOR.unwrap_or(board_config.zenoh_locator))\n                .domain_id(board_config.domain_id)\n                .node_name(nros_generated::SYSTEM.default_node_name()){extra};\n            run_system(config)\n        }},\n    )\n}}",
            crate = entry.crate_name,
            extra = entry.closure_extra,
        ));
    }
    out
}

fn render_zephyr_cmake(options: &GenerateOptions) -> String {
    ZEPHYR_CMAKE_TEMPLATE.replace("{{ package_name }}", &options.package_name)
}

fn render_zephyr_prj_conf(plan: &NrosPlan) -> String {
    // Phase 173.7 — append the net config derived from nros.toml
    // `[[transport]]` as an additive fragment. The base prj.conf
    // (kernel + generic networking) is the board's; nano-ros only adds
    // the *net knobs* (static IP / DHCP). No transport ⇒ no fragment ⇒
    // byte-identical base.
    format!(
        "{}{}",
        ZEPHYR_PRJ_CONF_TEMPLATE,
        zephyr_net_fragment(&plan.build)
    )
}

/// Phase 173.7 — Zephyr `CONFIG_NET_CONFIG_*` lines from the ethernet
/// transport's `ip` (`"dhcp"` or `"<addr>/<prefix>"`). Empty when no
/// ethernet transport / no `ip` is declared.
fn zephyr_net_fragment(build: &PlanBuildOptions) -> String {
    let Some(eth) = build
        .transports
        .iter()
        .find(|t| t.kind == TransportKind::Ethernet)
    else {
        return String::new();
    };
    let Some(ip) = eth.ip.as_deref() else {
        return String::new();
    };
    let mut out = String::from(
        "\n# Phase 173.7 — net config from nros.toml [[transport]] (additive).\n\
         CONFIG_NET_CONFIG_SETTINGS=y\n",
    );
    if ip.eq_ignore_ascii_case("dhcp") {
        out.push_str("CONFIG_NET_DHCPV4=y\n");
    } else {
        let (addr, prefix) = ip.split_once('/').unwrap_or((ip, "24"));
        out.push_str(&format!("CONFIG_NET_CONFIG_MY_IPV4_ADDR=\"{addr}\"\n"));
        if let Some(mask) = prefix_to_netmask(prefix) {
            out.push_str(&format!("CONFIG_NET_CONFIG_MY_IPV4_NETMASK=\"{mask}\"\n"));
        }
    }
    out
}

/// IPv4 prefix length → dotted netmask (`24` → `255.255.255.0`).
fn prefix_to_netmask(prefix: &str) -> Option<String> {
    let bits: u32 = prefix.parse().ok()?;
    if bits > 32 {
        return None;
    }
    let mask: u32 = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - bits)
    };
    Some(format!(
        "{}.{}.{}.{}",
        (mask >> 24) & 0xff,
        (mask >> 16) & 0xff,
        (mask >> 8) & 0xff,
        mask & 0xff
    ))
}

/// Dotted IPv4 → NuttX defconfig hex literal (`"10.0.2.50"` →
/// `"0x0a000232"`). `None` on malformed input.
fn ipv4_to_hex(addr: &str) -> Option<String> {
    let mut octets = [0u8; 4];
    let mut n = 0;
    for part in addr.split('.') {
        if n == 4 {
            return None;
        }
        octets[n] = part.parse().ok()?;
        n += 1;
    }
    if n != 4 {
        return None;
    }
    Some(format!(
        "0x{:02x}{:02x}{:02x}{:02x}",
        octets[0], octets[1], octets[2], octets[3]
    ))
}

/// Phase 173.7 — NuttX `CONFIG_NETINIT_*` defconfig fragment from the
/// ethernet transport's `ip`. `None` (no file emitted) unless the plan
/// targets NuttX *and* declares an ethernet transport with an `ip` —
/// keeping the no-transport NuttX build byte-identical (no extra file).
fn nuttx_net_fragment(plan: &NrosPlan) -> Option<String> {
    if profile(&plan.build.board, &plan.build.target).map(|p| p.kind) != Some(PlatformKind::Nuttx) {
        return None;
    }
    let eth = plan
        .build
        .transports
        .iter()
        .find(|t| t.kind == TransportKind::Ethernet)?;
    let ip = eth.ip.as_deref()?;
    let mut out = String::from(
        "# Phase 173.7 — NuttX net config from nros.toml [[transport]].\n\
         # Additive fragment — merge into the board defconfig (NuttX is\n\
         # built out-of-tree). nano-ros emits only the net knobs; kernel\n\
         # config stays the board's.\n\
         CONFIG_NET=y\n\
         CONFIG_NET_IPv4=y\n",
    );
    if ip.eq_ignore_ascii_case("dhcp") {
        out.push_str("CONFIG_NETINIT_DHCPC=y\n");
    } else {
        let (addr, prefix) = ip.split_once('/').unwrap_or((ip, "24"));
        if let Some(hex) = ipv4_to_hex(addr) {
            out.push_str(&format!("CONFIG_NETINIT_IPADDR={hex}\n"));
        }
        if let Some(mask) = prefix_to_netmask(prefix).as_deref().and_then(ipv4_to_hex) {
            out.push_str(&format!("CONFIG_NETINIT_NETMASK={mask}\n"));
        }
    }
    Some(out)
}

/// Phase 126.M5.nuttx — pin nightly + `rust-src` for targets that use
/// `-Z build-std`. NuttX `armv7a-nuttx-eabihf` rebuilds `std` from
/// source against the patched libc fork; the nightly date MUST match
/// `third-party/nuttx/libc/Cargo.toml`'s `version =` field per the
/// note in `examples/qemu-arm-nuttx/rust-toolchain.toml`. Other
/// platforms use stable rustc with prebuilt targets.
fn render_rust_toolchain(plan: &NrosPlan) -> Option<String> {
    // Phase 173.2 / 173.6 — toolchain pin driven by `profile()`. ESP32-C3
    // and NuttX need a nightly + `rust-src` pin (for `-Z build-std`);
    // ESP32-S3 (Xtensa) needs the espup `esp` channel; every other
    // platform uses stable.
    match profile(&plan.build.board, &plan.build.target) {
        Some(PlatformProfile {
            toolchain: Toolchain::Esp,
            ..
        }) => Some(
            r#"# Auto-generated by `nros build` for the ESP32-S3 (Xtensa) target.
# Phase 173.6 — xtensa-esp32s3-none-elf is not a rustup target; it ships
# in the espup `esp` channel, which also bundles `rust-src` for the
# `-Z build-std` (no_std + alloc) build. Install with `espup install`.
[toolchain]
channel = "esp"
components = ["rust-src", "rustfmt"]
"#
            .to_string(),
        ),
        Some(PlatformProfile {
            toolchain: Toolchain::Nightly,
            kind: PlatformKind::Esp32,
            ..
        }) => Some(
            r#"# Auto-generated by `nros build` for the ESP32-C3 target.
# Phase 126.M5.esp32 — riscv32imc-unknown-none-elf needs `-Z build-std`
# (no_std + alloc), which needs nightly + `rust-src`. Pin matches
# `tools/rust-toolchain.toml`.
[toolchain]
channel = "nightly-2026-04-11"
components = ["rust-src", "rustfmt"]
"#
            .to_string(),
        ),
        Some(PlatformProfile {
            toolchain: Toolchain::Nightly,
            kind: PlatformKind::Nuttx,
            ..
        }) => Some(
            r#"# Auto-generated by `nros build` for the NuttX target.
# Phase 126.M5.nuttx — armv7a-nuttx-eabihf needs `-Z build-std`, which
# needs nightly + `rust-src`. The pinned date matches the patched libc
# version in `third-party/nuttx/libc/Cargo.toml`.
[toolchain]
channel = "nightly-2026-04-11"
components = ["rust-src", "rustfmt"]
"#
            .to_string(),
        ),
        _ => None,
    }
}

fn render_build_rs(options: &GenerateOptions, plan: &NrosPlan) -> String {
    let generated_tables = render_generated_tables(plan);
    BUILD_TEMPLATE
        .replace("{{ plan_path }}", &path_for_template(&options.plan_path))
        .replace(
            "{{ native_link_directives }}",
            &render_native_link_directives(options, plan),
        )
        .replace(
            "{{ platform_link_directives }}",
            &render_platform_link_directives(plan),
        )
        .replace(
            "{{ generated_tables_literal }}",
            &format!("{generated_tables:?}"),
        )
}

/// Phase 126.M5.nuttx — emit build.rs link directives for target
/// platforms that need to link external kernel/userspace libs into
/// the final ELF. NuttX (Cortex-A7) needs the staging libs at
/// `$NUTTX_DIR/staging/lib{c,sched,drivers,...}.a` plus
/// arch-specific glue + the dramboot linker script. Mirrors what
/// `examples/qemu-arm-nuttx/rust/zenoh/talker/build.rs` emits per
/// crate.
fn render_platform_link_directives(plan: &NrosPlan) -> String {
    match profile(&plan.build.board, &plan.build.target).map(|p| p.link_kind) {
        Some(LinkKind::NuttxStaging) => NUTTX_LINK_DIRECTIVES.to_string(),
        _ => String::new(),
    }
}

const NUTTX_LINK_DIRECTIVES: &str = r#"    println!("cargo:rerun-if-env-changed=NUTTX_DIR");
    if let Ok(nuttx_dir) = env::var("NUTTX_DIR") {
        let nuttx_dir = PathBuf::from(nuttx_dir);
        let staging = nuttx_dir.join("staging");
        if staging.join("libc.a").exists() {
            // Preprocess the dramboot linker script (it #includes <nuttx/config.h>).
            let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"));
            let processed_ld = out_dir.join("dramboot.ld");
            let linker_script = nuttx_dir.join("boards/arm/qemu/qemu-armv7a/scripts/dramboot.ld");
            let status = Command::new("arm-none-eabi-gcc")
                .args([
                    "-E", "-P", "-x", "c",
                    &format!("-isystem{}", nuttx_dir.join("include").display()),
                    "-D__NuttX__", "-D__KERNEL__",
                    &format!("-I{}", nuttx_dir.join("arch/arm/src/chip").display()),
                    &format!("-I{}", nuttx_dir.join("arch/arm/src/common").display()),
                    &format!("-I{}", nuttx_dir.join("arch/arm/src/armv7-a").display()),
                    &format!("-I{}", nuttx_dir.join("sched").display()),
                ])
                .arg(&linker_script)
                .arg("-o")
                .arg(&processed_ld)
                .status()
                .expect("failed to preprocess linker script");
            assert!(status.success(), "linker script preprocessing failed");

            let board_src = nuttx_dir.join("arch/arm/src/board");
            let vectortab = nuttx_dir.join("arch/arm/src/arm_vectortab.o");
            let gcc_out = Command::new("arm-none-eabi-gcc")
                .args([
                    "-mcpu=cortex-a7",
                    "-mfloat-abi=hard",
                    "-mfpu=neon-vfpv4",
                    "-print-libgcc-file-name",
                ])
                .output()
                .expect("failed to find libgcc");
            let libgcc = String::from_utf8(gcc_out.stdout).unwrap().trim().to_string();

            println!("cargo:rustc-link-arg=-T{}", processed_ld.display());
            println!("cargo:rustc-link-arg=--entry=__start");
            println!("cargo:rustc-link-arg=-nostartfiles");
            println!("cargo:rustc-link-arg=-nodefaultlibs");
            println!("cargo:rustc-link-arg={}", vectortab.display());
            println!("cargo:rustc-link-arg=-L{}", staging.display());
            println!("cargo:rustc-link-arg=-L{}", board_src.display());
            println!("cargo:rustc-link-arg=-Wl,--start-group");
            for lib in [
                "sched", "drivers", "boards", "c", "mm", "arch", "xx", "apps", "net",
                "crypto", "fs", "binfmt", "openamp", "board",
            ] {
                println!("cargo:rustc-link-arg=-l{lib}");
            }
            println!("cargo:rustc-link-arg={libgcc}");
            println!("cargo:rustc-link-arg=-Wl,--end-group");
            println!("cargo:rerun-if-changed={}", linker_script.display());
        }
    }
"#;

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

fn render_cargo_config(options: &GenerateOptions, plan: &NrosPlan) -> Option<String> {
    let nros_path = options.nros_path.as_path();
    let p = profile(&plan.build.board, &plan.build.target)?;
    match p.kind {
        // Phase 126.M5.esp32 — ESP32 bare-metal target wiring. esp-hal
        // links via `-Tlinkall.x` (esp-hal ships the linker script);
        // `force-frame-pointers` matches the example slices.
        // `build-std = ["core", "alloc"]` because esp32 is no_std + alloc.
        // ESP32-C3 uses stable riscv32imc; ESP32-S3 needs the `+esp`
        // Xtensa nightly (handled by render_rust_toolchain).
        PlatformKind::Esp32 => {
            let target = esp32_target(p.chip?);
            Some(format!(
                r#"[build]
target = "{target}"

[target.{target}]
rustflags = [
    "-C", "link-arg=-Tlinkall.x",
    "-C", "force-frame-pointers",
]

[env]
ESP_LOG = "info"

[unstable]
build-std = ["core", "alloc"]
"#
            ))
        }
        // Phase 126.M5.stm32f4 — STM32F4 (Cortex-M4F, thumbv7em-none-eabihf).
        // cortex-m-rt's `link.x` placed via the board crate's memory.x;
        // diagnostics go over defmt-rtt (no QEMU runner — STM32F4 is real
        // hardware flashed with probe-rs, so the e2e test asserts the build
        // artifact only).
        PlatformKind::Stm32 => Some(
            r#"[build]
target = "thumbv7em-none-eabihf"

[target.thumbv7em-none-eabihf]
rustflags = [
    "-C", "link-arg=-Tlink.x",
]
"#
            .to_string(),
        ),
        // Phase 126.M5.threadx-riscv64 — bare-metal ThreadX on QEMU RISC-V
        // virt (riscv64gc-unknown-none-elf). `link.lds` is emitted to the
        // board crate's OUT_DIR + surfaced via `cargo:rustc-link-search`
        // (which propagates), so the rustflag references it by name. The
        // ThreadX kernel + NetX C builds need the RV64 port + config dirs;
        // the example pins them via env (the threadx-linux defaults in
        // `.envrc` point at the wrong port). Absolute paths so the
        // generated package (built out-of-tree) resolves them.
        PlatformKind::ThreadxRiscv64 => {
            let workspace = nros_path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)?;
            let config_dir = path_for_template(
                &workspace.join("packages/boards/nros-board-threadx-qemu-riscv64/config"),
            );
            let extra_includes = path_for_template(
                &workspace
                    .join("third-party/threadx/kernel/ports/risc-v64/gnu/example_build/qemu_virt"),
            );
            Some(format!(
                r#"[build]
target = "riscv64gc-unknown-none-elf"

[target.riscv64gc-unknown-none-elf]
rustflags = [
    "-C", "link-arg=-Tlink.lds",
    "-C", "link-arg=--nmagic",
]

[env]
NETX_CONFIG_DIR = {{ value = "{config_dir}", force = true }}
THREADX_CONFIG_DIR = {{ value = "{config_dir}", force = true }}
THREADX_PORT = {{ value = "risc-v64/gnu", force = true }}
THREADX_EXTRA_INCLUDES = {{ value = "{extra_includes}", force = true }}
"#
            ))
        }
        PlatformKind::Freertos => Some(
            r#"[target.thumbv7m-none-eabi]
runner = "qemu-system-arm -cpu cortex-m3 -machine mps2-an385 -nographic -semihosting-config enable=on,target=native -kernel"
rustflags = [
    "-C", "link-arg=-Tmps2_an385.ld",
    "-C", "link-arg=--nmagic",
]
"#
            .to_string(),
        ),
        // Phase 126.M5.bare-metal — pure Cortex-M3. cortex-m-rt's
        // `link.x` linker script (pulled in via the board crate's
        // memory.x) places the vector table + sections; the QEMU
        // runner boots the ELF as an mps2-an385 kernel image with
        // semihosting for stdout + exit.
        PlatformKind::BareMetal => Some(
            r#"[target.thumbv7m-none-eabi]
runner = "qemu-system-arm -cpu cortex-m3 -machine mps2-an385 -nographic -semihosting-config enable=on,target=native -kernel"
rustflags = [
    "-C", "link-arg=-Tlink.x",
]
"#
            .to_string(),
        ),
        // Phase 126.M5.nuttx — NuttX QEMU ARM target wiring.
        // armv7a-nuttx-eabihf needs build-std (Rust stdlib rebuilt
        // against NuttX's libc), the cortex-a7 + neon-vfpv4 ABI
        // flags, and the in-tree libc fork patched in via
        // `[patch.crates-io]`. The `[patch.crates-io]` block MUST
        // live in `.cargo/config.toml` (NOT `Cargo.toml`) so that
        // `-Z build-std` applies it to the stdlib build itself —
        // `Cargo.toml`'s `[patch]` only affects the consumer's deps.
        // The board crate's build.rs handles CFLAGS / linker-script
        // discovery from `$NUTTX_DIR`.
        PlatformKind::Nuttx => {
            let workspace = nros_path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)?;
            Some(format!(
                r#"[build]
target = "armv7a-nuttx-eabihf"

[unstable]
build-std = ["std", "panic_abort"]
build-std-features = ["compiler-builtins-mem"]

[target.armv7a-nuttx-eabihf]
linker = "arm-none-eabi-gcc"
rustflags = [
    "-C", "link-arg=-mcpu=cortex-a7",
    "-C", "link-arg=-mfloat-abi=hard",
    "-C", "link-arg=-mfpu=neon-vfpv4",
]

[env]
CC_armv7a_nuttx_eabihf = "arm-none-eabi-gcc"
CFLAGS_armv7a_nuttx_eabihf = "-mcpu=cortex-a7 -mfloat-abi=hard -mfpu=neon-vfpv4"

[patch.crates-io]
libc = {{ path = "{}" }}
"#,
                path_for_template(&workspace.join("third-party/nuttx/libc")),
            ))
        }
        PlatformKind::Posix
        | PlatformKind::Zephyr
        | PlatformKind::ThreadxLinux
        | PlatformKind::OrinSpe => None,
    }
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

/// Phase 173.5 — board transport Cargo features from `[[transport]]`,
/// deduped (`ethernet` / `serial` / `can`). Empty when no transports
/// declared.
fn transport_cargo_features(build: &PlanBuildOptions) -> Vec<String> {
    let mut feats: Vec<String> = Vec::new();
    for t in &build.transports {
        let f = t.kind.cargo_feature().to_string();
        if !feats.contains(&f) {
            feats.push(f);
        }
    }
    feats
}

/// Phase 173.5 — format a board crate path dep, merging the board's
/// intrinsic `base_features` (e.g. the stm32 chip) with the declared
/// transport features.
///
/// When `[[transport]]` is declared the board's **default** features are
/// disabled so the transport selection is authoritative (the
/// `ethernet`→`serial` swap, or a bridge's multi-transport set). With no
/// declared transports the dep is emitted exactly as pre-173.5 (board
/// defaults left on) — keeping existing generated manifests
/// byte-identical.
fn board_dep(name: &str, path: &str, base_features: &[&str], build: &PlanBuildOptions) -> String {
    let transports = transport_cargo_features(build);
    if transports.is_empty() {
        if base_features.is_empty() {
            format!("{name} = {{ path = \"{path}\" }}\n")
        } else {
            let base: Vec<String> = base_features.iter().map(|s| s.to_string()).collect();
            format!(
                "{name} = {{ path = \"{path}\", features = {} }}\n",
                toml_string_array(&base)
            )
        }
    } else {
        let mut feats: Vec<String> = base_features.iter().map(|s| s.to_string()).collect();
        for t in transports {
            if !feats.contains(&t) {
                feats.push(t);
            }
        }
        format!(
            "{name} = {{ path = \"{path}\", default-features = false, features = {} }}\n",
            toml_string_array(&feats)
        )
    }
}

fn render_platform_dependencies(options: &GenerateOptions, plan: &NrosPlan) -> String {
    let Some(workspace) = workspace_from_nros_path(&options.nros_path) else {
        return String::new();
    };
    let Some(p) = profile(&plan.build.board, &plan.build.target) else {
        return String::new();
    };
    match p.kind {
        // Phase 126.M5.esp32 — ESP32 boards pull the board crate + the
        // esp-hal entry/panic/bootloader crates. esp-hal's `#[main]` proc
        // macro + esp-backtrace's panic handler + esp-bootloader's
        // `esp_app_desc!()` must be visible at the generated package's
        // crate root, so they're direct deps (not just transitive via the
        // board crate). The chip feature (`esp32c3` / `esp32s3`) gates each.
        PlatformKind::Esp32 => {
            let chip = p.chip.unwrap_or("esp32c3");
            // Phase 173.6 — the board crate is chip-specific: ESP32-C3
            // runs under QEMU (OpenETH NIC, ethernet/serial); ESP32-S3 is
            // real Xtensa hardware (serial). Selected from `profile().chip`,
            // not a new match arm.
            let board_crate = if chip == "esp32s3" {
                "nros-board-esp32s3"
            } else {
                "nros-board-esp32-qemu"
            };
            let board = board_dep(
                board_crate,
                &path_for_template(&workspace.join(format!("packages/boards/{board_crate}"))),
                &[],
                &plan.build,
            );
            format!(
                "{board}\
                 esp-hal = {{ version = \"~1.0.0\", features = [\"{chip}\", \"unstable\"] }}\n\
                 esp-backtrace = {{ version = \"~0.18.0\", features = [\"{chip}\", \"panic-handler\", \"println\"] }}\n\
                 esp-bootloader-esp-idf = {{ version = \"~0.4.0\", features = [\"{chip}\"] }}\n",
            )
        }
        // Phase 126.M5.stm32f4 — STM32F4 boards pull the board crate (with
        // the chip feature) + the defmt logging + panic-probe crates.
        // defmt's `timestamp!` macro + panic-probe's panic handler +
        // defmt-rtt's transport must be visible at the generated package's
        // crate root, so they're direct deps (not just transitive via the
        // board crate).
        PlatformKind::Stm32 => {
            let chip = p.chip.unwrap_or("stm32f429");
            let board = board_dep(
                "nros-board-stm32f4",
                &path_for_template(&workspace.join("packages/boards/nros-board-stm32f4")),
                &[chip],
                &plan.build,
            );
            format!(
                "{board}\
                 panic-probe = {{ version = \"0.3\", features = [\"print-defmt\"] }}\n\
                 defmt = \"0.3\"\n\
                 defmt-rtt = \"0.4\"\n",
            )
        }
        PlatformKind::Posix => format!(
            "nros-platform-cffi = {{ path = \"{}\", default-features = false, features = [\"posix-c-port\"] }}\n",
            path_for_template(&workspace.join("packages/core/nros-platform-cffi")),
        ),
        PlatformKind::Freertos => format!(
            "{}panic-semihosting = {{ version = \"0.6\", features = [\"exit\"] }}\n",
            board_dep(
                "nros-board-mps2-an385-freertos",
                &path_for_template(
                    &workspace.join("packages/boards/nros-board-mps2-an385-freertos")
                ),
                &[],
                &plan.build,
            ),
        ),
        // Phase 126.M5.bare-metal — pure Cortex-M3 (MPS2-AN385,
        // thumbv7m-none-eabi). The board crate owns hardware + lwIP +
        // smoltcp init and re-exports the `cortex-m-rt` `#[entry]`
        // macro; panic-semihosting provides the `no_std` panic
        // handler + QEMU exit.
        PlatformKind::BareMetal => format!(
            "{}panic-semihosting = {{ version = \"0.6\", features = [\"exit\"] }}\n",
            board_dep(
                "nros-board-mps2-an385",
                &path_for_template(&workspace.join("packages/boards/nros-board-mps2-an385")),
                &[],
                &plan.build,
            ),
        ),
        // Phase 126.M5.nuttx — NuttX QEMU ARM (Cortex-A7 + virtio-net,
        // armv7a-nuttx-eabihf target). The board crate provides the
        // BoardInit shim; NuttX kernel + virtio-net + BSD sockets are
        // built out-of-tree via `just nuttx setup`.
        PlatformKind::Nuttx => format!(
            "nros-board-nuttx-qemu-arm = {{ path = \"{}\" }}\n",
            path_for_template(&workspace.join("packages/boards/nros-board-nuttx-qemu-arm")),
        ),
        // Phase 126.M5.zephyr — zephyr-lang-rust integration. The
        // generated package consumes the Zephyr Rust API through the
        // `zephyr` crate (provides `set_logger`, `kconfig`, POSIX
        // shims, the network-readiness wait helper). The kernel +
        // RMW + nros C runtime are linked at the CMake layer through
        // `rust_cargo_application()`.
        PlatformKind::Zephyr => "zephyr = \"0.1.0\"\nlog = \"0.4\"\n".to_string(),
        // Phase 126.M5.threadx — ThreadX board crate. Two variants:
        // `threadx-linux` (host-hosted ThreadX + NetX Duo over the
        // NSOS BSD shim; builds as a normal Linux executable) and
        // `threadx-qemu-riscv64` (bare-metal riscv64gc, QEMU virt +
        // virtio-net). Both board crates own their kernel / NetX link
        // via propagating `cargo:rustc-link-lib`, so the generated
        // package needs only the path dep — no consumer-side build.rs
        // link directives.
        PlatformKind::ThreadxRiscv64 => board_dep(
            "nros-board-threadx-qemu-riscv64",
            &path_for_template(&workspace.join("packages/boards/nros-board-threadx-qemu-riscv64")),
            &[],
            &plan.build,
        ),
        PlatformKind::ThreadxLinux => board_dep(
            "nros-board-threadx-linux",
            &path_for_template(&workspace.join("packages/boards/nros-board-threadx-linux")),
            &[],
            &plan.build,
        ),
        PlatformKind::OrinSpe => String::new(),
    }
}

/// Canonical RMW name (`zenoh` / `xrce` / `cyclonedds`) from any of the
/// accepted token spellings. `None` for empty / unknown.
fn normalize_rmw(rmw: &str) -> Option<&'static str> {
    match rmw {
        "zenoh" | "rmw-zenoh" | "rmw-zenoh-cffi" => Some("zenoh"),
        "xrce" | "rmw-xrce" | "rmw-xrce-cffi" => Some("xrce"),
        "cyclonedds" | "rmw-cyclonedds" | "rmw-cyclonedds-cffi" => Some("cyclonedds"),
        _ => None,
    }
}

/// Phase 173.5 — the set of canonical RMW backends the build links: the
/// union of every `[[transport]].rmw` (falling back to `build.rmw` when
/// a transport omits it), deduped. With no transports declared it is
/// just `build.rmw` — so single-RMW builds are byte-identical.
fn rmw_set(build: &PlanBuildOptions) -> Vec<&'static str> {
    let mut set: Vec<&'static str> = Vec::new();
    let raw: Vec<&str> = if build.transports.is_empty() {
        vec![build.rmw.as_str()]
    } else {
        build
            .transports
            .iter()
            .map(|t| t.rmw.as_deref().unwrap_or(build.rmw.as_str()))
            .collect()
    };
    for r in raw {
        if let Some(n) = normalize_rmw(r)
            && !set.contains(&n)
        {
            set.push(n);
        }
    }
    set
}

/// Cargo dep line(s) for one canonical RMW backend.
fn render_one_backend(workspace: &Path, build: &PlanBuildOptions, rmw: &str) -> String {
    match rmw {
        "zenoh" => format!(
            "nros-rmw-zenoh = {{ path = \"{}\", default-features = false, features = {} }}\n",
            path_for_template(&workspace.join("packages/zpico/nros-rmw-zenoh")),
            toml_string_array(&backend_features(build, "zenoh")),
        ),
        "xrce" => format!(
            "nros-rmw-xrce-cffi = {{ path = \"{}\", default-features = false, features = {} }}\n",
            path_for_template(&workspace.join("packages/xrce/nros-rmw-xrce-cffi")),
            toml_string_array(&backend_features(build, "xrce")),
        ),
        // Phase 169 (nano-ros 2026-05-19) — dust-dds retired; the
        // generic "dds" / "rmw-dds" / "rmw-dds-cffi" tokens are no
        // longer wired up. Cyclone is the DDS backend and is
        // selected via "cyclonedds" only (see nano-ros Phase 169.5).
        "cyclonedds" => "# Cyclone DDS is a CMake/C++ project — no Rust shim crate.\n\
             # Consumers select it via NANO_ROS_RMW=cyclonedds at the CMake\n\
             # layer (nros-c / nros-cpp). The generated Cargo.toml leaves\n\
             # the DDS slot empty; the staticlib is linked into the binary\n\
             # by the CMake glue alongside `corrosion_link_libraries`.\n"
            .to_string(),
        _ => String::new(),
    }
}

fn render_backend_dependencies(options: &GenerateOptions, plan: &NrosPlan) -> String {
    let Some(workspace) = workspace_from_nros_path(&options.nros_path) else {
        return String::new();
    };
    // Phase 173.5 — emit a dep for every RMW the transports bind to
    // (bridge mode links 2+). Single-RMW (or no `[[transport]]`) emits
    // exactly one, byte-identical to before.
    rmw_set(&plan.build)
        .iter()
        .map(|rmw| render_one_backend(&workspace, &plan.build, rmw))
        .collect()
}

fn workspace_from_nros_path(nros_path: &Path) -> Option<PathBuf> {
    nros_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

fn backend_features(build: &PlanBuildOptions, backend: &str) -> Vec<String> {
    let mut features = Vec::new();
    if uses_std(build) {
        features.push("std".to_string());
    }
    if let Some(platform) = platform_feature(&build.board, &build.target) {
        features.push(platform.to_string());
    }
    // Phase 126.M4 — `link-tcp` / `link-udp-unicast` feature gates were
    // deleted from zpico-sys (CLAUDE.md "Key Patterns": "vendor always
    // compiles those transports; locator picks at runtime"). nros-rmw-zenoh
    // now only exposes `link-tls` + `link-custom`. Plain TCP/UDP is
    // unconditional — no per-backend feature needed.
    let _ = backend;
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

fn generated_default_features(
    build: &PlanBuildOptions,
    managed_lifecycle: bool,
    param_persistence: bool,
) -> Vec<String> {
    let mut features = Vec::new();
    if uses_std(build) {
        features.push("std".to_string());
    }
    // Phase 172.A — a `[lifecycle]` plan needs the REP-2002 services on the
    // executor (`nros/lifecycle-services` → `nros-node/lifecycle-services`).
    if managed_lifecycle {
        features.push("nros/lifecycle-services".to_string());
    }
    // Phase 172.H — a `[param_persistence]` plan needs the parameter services
    // (`nros/param-services`); the generated runtime declares params, registers
    // the services, and attaches the persistence backend.
    if param_persistence {
        features.push("nros/param-services".to_string());
    }
    // Phase 173.2 — `nros/<feature>` + the per-platform local aliases
    // (which gate the platform-specific Cargo deps + cfg) both come from
    // the single `profile()` descriptor. (Since Phase 173.2b the
    // `src/main.rs` entry is selected by `render_main` from
    // `profile().board_entry`, not by these feature aliases.) ESP32/STM32
    // carry only their own alias (`platform-esp32-qemu` /
    // `platform-stm32`), NOT the `platform-bare-metal` alias.
    if let Some(p) = profile(&build.board, &build.target) {
        features.push(format!("nros/{}", p.nros_platform_feature));
        for alias in p.local_aliases {
            features.push(alias.to_string());
        }
    }
    if uses_rmw_cffi(&build.rmw) {
        features.push("nros/rmw-cffi".to_string());
        features.push("nros-orchestration/rmw-cffi".to_string());
        if let Some(rmw) = rmw_backend_feature(&build.rmw) {
            features.push(format!("nros/{rmw}"));
        }
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

// ============================================================================
// Phase 173.2 — PlatformProfile descriptor + single resolver
// ----------------------------------------------------------------------------
// One `profile(board, target)` lookup replaces the per-render-function
// re-matching of `platform_feature` + `esp32_chip` + `stm32_chip`. The
// descriptor centralizes every platform's *static* metadata (nros
// feature, default-feature aliases, entry/link/net/toolchain kinds);
// the text-heavy render bodies (cargo config, deps) dispatch on
// `PlatformKind` and interpolate chip / workspace paths from the
// profile. Adding a platform = one `profile()` arm + the body hooks.
// ============================================================================

/// The resolved platform identity for a `(board, target)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlatformKind {
    Posix,
    Freertos,
    BareMetal,
    Nuttx,
    Zephyr,
    ThreadxLinux,
    ThreadxRiscv64,
    Esp32,
    Stm32,
    OrinSpe,
}

/// Rust toolchain a generated package pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toolchain {
    /// Stable rustc with a prebuilt target — no `rust-toolchain.toml`.
    Stable,
    /// Pinned nightly + `rust-src` for `-Z build-std`.
    Nightly,
    /// Xtensa `+esp` espup toolchain (ESP32-S3) — not emitted yet.
    Esp,
}

/// External libraries the generated `build.rs` must link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkKind {
    /// Board crate / cargo handles all linking.
    None,
    /// NuttX staging-archive group-link + dramboot linker script.
    NuttxStaging,
}

/// Shape of the generated package's entry point (`src/main.rs` /
/// `src/lib.rs`). `render_main` branches on this (and on `board_entry`)
/// to emit one entry shape, replacing the per-platform `#[cfg]` blocks
/// the old `main.rs.jinja` shipped (Phase 173.2b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    /// Hosted Rust `fn main` (posix / threadx-linux host).
    HostedMain,
    /// `<board>::run(cfg, closure)` on a bare-metal / RTOS target.
    BoardRun,
    /// Rust staticlib consumed by zephyr-lang-rust `rust_cargo_application()`.
    ZephyrStaticlib,
}

/// Who owns NIC + IP bring-up (Phase 173.7 emit path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetStack {
    /// RTOS brings up the stack (Zephyr / NuttX); generator emits an
    /// additive RTOS-config fragment.
    RtosOwned,
    /// Board crate owns the stack (smoltcp / lwIP / NetX / esp-hal);
    /// `nros.toml` values flow into the board `Config`.
    NanoRosOwned,
}

/// Static descriptor for a resolved platform. `chip` carries the
/// esp-hal / stm32 chip feature when applicable (drives dep + target
/// interpolation in the render bodies).
#[derive(Debug, Clone, Copy)]
struct PlatformProfile {
    kind: PlatformKind,
    /// The `nros/<feature>` selected (e.g. `platform-posix`). ESP32 and
    /// STM32 both map onto `platform-bare-metal`.
    nros_platform_feature: &'static str,
    /// Extra local default-feature aliases beyond `nros/<feature>` (gate
    /// the platform-specific Cargo deps + cfg). Note: since Phase 173.2b
    /// the `src/main.rs` entry shape is chosen by `render_main` from
    /// `board_entry`, not by these aliases.
    local_aliases: &'static [&'static str],
    toolchain: Toolchain,
    link_kind: LinkKind,
    entry_kind: EntryKind,
    #[allow(dead_code)] // consumed by Phase 173.7 emit path
    net_stack: NetStack,
    /// esp-hal (`esp32c3`/`esp32s3`) or stm32 (`stm32f429`/`stm32f407`)
    /// chip feature; `None` for non-chip platforms.
    chip: Option<&'static str>,
    /// Phase 173.2b — `src/main.rs` entry shape. `None` for hosted
    /// native/posix (which calls `run_system` directly via `fn main`);
    /// `Some(spec)` for board-driven entries (bare-metal / RTOS hosts
    /// whose board rlib's `run()` boots hardware then drives the user
    /// closure). The hosted threadx-linux host is `HostedMain` yet still
    /// carries a `BoardEntry` because it boots the ThreadX kernel via
    /// `nros_board_threadx_linux::run`.
    board_entry: Option<BoardEntry>,
}

/// Phase 173.2b — the per-board pieces `render_main` interpolates into
/// the shared board-run entry shape. Everything else (the `run_system`
/// helper, the `ExecutorConfig::new(..).domain_id(..).node_name(..)`
/// chain, the closure scaffolding) is identical across boards.
#[derive(Debug, Clone, Copy)]
struct BoardEntry {
    /// Board rlib invoked as `<crate>::run(<crate>::Config::default(), ..)`.
    crate_name: &'static str,
    /// Doc comment emitted directly above the entry fn.
    comment: &'static str,
    /// Attribute(s) + `fn` signature line(s) preceding the fn body
    /// (e.g. `#[nros_board_mps2_an385::entry]\nfn main() -> !`).
    signature: &'static str,
    /// Panic-handler / log-transport `use`s (and any other crate-root
    /// items) pinned at the crate root above the entry.
    crate_root_extra: &'static str,
    /// Builder-chain suffix appended inside the closure (e.g. esp32's
    /// `.clock_us(...)`); empty for boards with no extra config.
    closure_extra: &'static str,
}

/// Pure Cortex-M3 (MPS2-AN385). Entry via cortex-m-rt's `#[entry]`.
const BOARD_ENTRY_BARE_METAL: BoardEntry = BoardEntry {
    crate_name: "nros_board_mps2_an385",
    comment: "\
// Phase 126.M5.bare-metal — pure Cortex-M3 (MPS2-AN385). Entry comes
// from cortex-m-rt's `#[entry]` (re-exported by the board crate).
// The board crate's `run()` does hardware + smoltcp/lwIP init then
// invokes the closure; referencing it pins the board rlib + its
// linker-script / vector-table contributions into the image.",
    signature: "#[nros_board_mps2_an385::entry]\nfn main() -> !",
    crate_root_extra: "use panic_semihosting as _;",
    closure_extra: "",
};

/// STM32F4 (Cortex-M4F). Entry via cortex-m-rt's `#[entry]`.
const BOARD_ENTRY_STM32: BoardEntry = BoardEntry {
    crate_name: "nros_board_stm32f4",
    comment: "\
// Phase 126.M5.stm32f4 — STM32F4 (Cortex-M4F). Entry via cortex-m-rt's
// `#[entry]` (re-exported by the board crate). `run()` does clock +
// Ethernet + smoltcp init then invokes the closure; referencing it
// pins the board rlib + its linker-script / vector-table
// contributions into the image. Diagnostics flow over defmt-rtt.",
    signature: "#[nros_board_stm32f4::entry]\nfn main() -> !",
    crate_root_extra: "\
use defmt_rtt as _;
use panic_probe as _;
defmt::timestamp!(\"{=u64:us}\", { 0 });",
    closure_extra: "",
};

/// ThreadX-Linux host: board `run()` boots ThreadX + NetX Duo on the
/// application thread (hosted `fn main`, not bare-metal).
const BOARD_ENTRY_THREADX_LINUX: BoardEntry = BoardEntry {
    crate_name: "nros_board_threadx_linux",
    comment: "\
// Phase 126.M5.threadx — ThreadX-Linux is host-hosted: the board
// crate's `run()` boots the ThreadX kernel + NetX Duo stack, then
// invokes the closure on the application thread. Referencing
// `nros_board_threadx_linux::run` is REQUIRED — it pins the board
// rlib (and its build-script-linked ThreadX kernel + NetX archives)
// into the link graph so `--gc-sections` doesn't drop the platform
// `nros_platform_*` / `_tx_*` symbols.",
    signature: "fn main() -> !",
    crate_root_extra: "",
    closure_extra: "",
};

/// Bare-metal ThreadX on QEMU RISC-V virt. Entry `_start -> main`.
const BOARD_ENTRY_THREADX_RISCV64: BoardEntry = BoardEntry {
    crate_name: "nros_board_threadx_qemu_riscv64",
    comment: "\
// Phase 126.M5.threadx-riscv64 — bare-metal ThreadX on QEMU RISC-V
// virt. Entry is `#[no_mangle] extern \"C\" fn main` (the board's
// `link.lds` jumps `_start -> main`). `run()` boots the ThreadX
// kernel + NetX Duo over virtio-net then invokes the closure;
// referencing it pins the board rlib + its kernel/NetX archives +
// the linker script into the image.",
    signature: "#[unsafe(no_mangle)]\nextern \"C\" fn main() -> !",
    crate_root_extra: "",
    closure_extra: "",
};

/// ESP32-C3 under QEMU (esp-hal). Entry via esp-hal's `#[main]`.
const BOARD_ENTRY_ESP32_QEMU: BoardEntry = BoardEntry {
    crate_name: "nros_board_esp32_qemu",
    comment: "\
// Phase 126.M5.esp32 — esp-hal `#[main]` entry. The board's `run()`
// initialises the chip + network + log writer, then drives the user
// closure and loops forever (ESP32 has no process exit). The board
// `Config` carries the zenoh locator + domain id (defaults from the
// board crate; override via a config.toml the generator could embed
// in a follow-up).",
    signature: "#[esp_hal::main]\nfn main() -> !",
    crate_root_extra: "\
use esp_backtrace as _;
nros_board_esp32_qemu::esp_bootloader_esp_idf::esp_app_desc!();",
    closure_extra: "\n                .clock_us(nros_board_esp32_qemu::nros_platform_esp32_qemu::clock::clock_us)",
};

// Phase 173.6 — ESP32-S3 (Xtensa) real-hardware entry. Same esp-hal
// `#[main]` shape as the C3-under-QEMU board, but the crate is
// `nros_board_esp32s3` (serial transport, no QEMU NIC).
const BOARD_ENTRY_ESP32S3: BoardEntry = BoardEntry {
    crate_name: "nros_board_esp32s3",
    comment: "\
// Phase 173.6 — ESP32-S3 esp-hal `#[main]` entry. The board's `run()`
// initialises the chip + serial transport + log writer, then drives the
// user closure and loops forever (ESP32 has no process exit).",
    signature: "#[esp_hal::main]\nfn main() -> !",
    crate_root_extra: "\
use esp_backtrace as _;
nros_board_esp32s3::esp_bootloader_esp_idf::esp_app_desc!();",
    closure_extra: "\n                .clock_us(nros_board_esp32s3::nros_platform_esp32s3::clock::clock_us)",
};

/// FreeRTOS on MPS2-AN385. Entry `extern "C" fn _start`.
const BOARD_ENTRY_FREERTOS: BoardEntry = BoardEntry {
    crate_name: "nros_board_mps2_an385_freertos",
    comment: "",
    signature: "#[unsafe(no_mangle)]\nextern \"C\" fn _start() -> !",
    crate_root_extra: "use panic_semihosting as _;",
    closure_extra: "",
};

/// Single board/target → profile resolver. The one place new platforms
/// register. `None` = unsupported `(board, target)`.
fn profile(board: &str, target: &str) -> Option<PlatformProfile> {
    let mk = |kind,
              nros_platform_feature,
              local_aliases: &'static [&'static str],
              toolchain,
              link_kind,
              entry_kind,
              net_stack,
              chip,
              board_entry| {
        Some(PlatformProfile {
            kind,
            nros_platform_feature,
            local_aliases,
            toolchain,
            link_kind,
            entry_kind,
            net_stack,
            chip,
            board_entry,
        })
    };
    match board {
        "native" | "posix" => mk(
            PlatformKind::Posix,
            "platform-posix",
            &[],
            Toolchain::Stable,
            LinkKind::None,
            EntryKind::HostedMain,
            NetStack::NanoRosOwned,
            None,
            None,
        ),
        "zephyr" => mk(
            PlatformKind::Zephyr,
            "platform-zephyr",
            &["platform-zephyr"],
            Toolchain::Stable,
            LinkKind::None,
            EntryKind::ZephyrStaticlib,
            NetStack::RtosOwned,
            None,
            None,
        ),
        "freertos" | "freeRTOS" | "FreeRTOS" => mk(
            PlatformKind::Freertos,
            "platform-freertos",
            &["platform-freertos"],
            Toolchain::Stable,
            LinkKind::None,
            EntryKind::BoardRun,
            NetStack::NanoRosOwned,
            None,
            Some(BOARD_ENTRY_FREERTOS),
        ),
        "nuttx" | "NuttX" => mk(
            PlatformKind::Nuttx,
            "platform-nuttx",
            &["platform-nuttx"],
            Toolchain::Nightly,
            LinkKind::NuttxStaging,
            EntryKind::BoardRun,
            NetStack::RtosOwned,
            None,
            // Phase 173.2b — NuttX is `BoardRun` but the legacy template
            // shipped no NuttX entry block, so the hosted `fn main`
            // (active for any non-`#[cfg]`-gated platform) drives it
            // today. `None` preserves that std hosted shape byte-for-byte;
            // a NuttX `BoardEntry` is a future follow-up.
            None,
        ),
        "threadx" | "ThreadX" => {
            if target.contains("riscv64") {
                mk(
                    PlatformKind::ThreadxRiscv64,
                    "platform-threadx",
                    &["platform-threadx-riscv64"],
                    Toolchain::Stable,
                    LinkKind::None,
                    EntryKind::BoardRun,
                    NetStack::NanoRosOwned,
                    None,
                    Some(BOARD_ENTRY_THREADX_RISCV64),
                )
            } else {
                mk(
                    PlatformKind::ThreadxLinux,
                    "platform-threadx",
                    &["platform-threadx"],
                    Toolchain::Stable,
                    LinkKind::None,
                    EntryKind::HostedMain,
                    NetStack::NanoRosOwned,
                    None,
                    Some(BOARD_ENTRY_THREADX_LINUX),
                )
            }
        }
        // ESP32 (esp-hal bare-metal) maps onto `platform-bare-metal`; the
        // chip-specific esp-hal deps + linker glue come from `chip`.
        "esp32-qemu" | "esp32" | "esp32c3" | "esp32-c3" => mk(
            PlatformKind::Esp32,
            "platform-bare-metal",
            &["platform-esp32-qemu"],
            Toolchain::Nightly,
            LinkKind::None,
            EntryKind::BoardRun,
            NetStack::NanoRosOwned,
            Some("esp32c3"),
            Some(BOARD_ENTRY_ESP32_QEMU),
        ),
        "esp32s3" | "esp32-s3" => mk(
            PlatformKind::Esp32,
            "platform-bare-metal",
            &["platform-esp32-qemu"],
            Toolchain::Esp,
            LinkKind::None,
            EntryKind::BoardRun,
            NetStack::NanoRosOwned,
            Some("esp32s3"),
            Some(BOARD_ENTRY_ESP32S3),
        ),
        // STM32F4 (Cortex-M4F) maps onto `platform-bare-metal`; the chip
        // board feature + defmt deps come from `chip`.
        "stm32f4" | "stm32f429" => mk(
            PlatformKind::Stm32,
            "platform-bare-metal",
            &["platform-stm32"],
            Toolchain::Stable,
            LinkKind::None,
            EntryKind::BoardRun,
            NetStack::NanoRosOwned,
            Some("stm32f429"),
            Some(BOARD_ENTRY_STM32),
        ),
        "stm32f407" => mk(
            PlatformKind::Stm32,
            "platform-bare-metal",
            &["platform-stm32"],
            Toolchain::Stable,
            LinkKind::None,
            EntryKind::BoardRun,
            NetStack::NanoRosOwned,
            Some("stm32f407"),
            Some(BOARD_ENTRY_STM32),
        ),
        "baremetal" | "bare-metal" => mk(
            PlatformKind::BareMetal,
            "platform-bare-metal",
            &["platform-bare-metal"],
            Toolchain::Stable,
            LinkKind::None,
            EntryKind::BoardRun,
            NetStack::NanoRosOwned,
            None,
            Some(BOARD_ENTRY_BARE_METAL),
        ),
        "orin-spe" => mk(
            PlatformKind::OrinSpe,
            "platform-orin-spe",
            &[],
            Toolchain::Stable,
            LinkKind::None,
            EntryKind::BoardRun,
            NetStack::NanoRosOwned,
            None,
            // Phase 173.2b — like NuttX, orin-spe is `BoardRun` but had no
            // legacy template entry block; the hosted `fn main` drives it.
            // `None` keeps that std hosted shape byte-identical.
            None,
        ),
        _ if target.contains("linux") => mk(
            PlatformKind::Posix,
            "platform-posix",
            &[],
            Toolchain::Stable,
            LinkKind::None,
            EntryKind::HostedMain,
            NetStack::NanoRosOwned,
            None,
            None,
        ),
        _ => None,
    }
}

fn platform_feature(board: &str, target: &str) -> Option<&'static str> {
    profile(board, target).map(|p| p.nros_platform_feature)
}

/// Phase 126.M5.esp32 — rustc target triple for an ESP32 chip feature
/// (`profile().chip`). ESP32-C3 runs under QEMU's Espressif fork on
/// `riscv32imc-unknown-none-elf`; ESP32-S3 needs `xtensa-esp32s3-none-elf`.
fn esp32_target(chip: &str) -> &'static str {
    match chip {
        "esp32s3" => "xtensa-esp32s3-none-elf",
        // esp32c3 (and any other RISC-V ESP32) use riscv32imc.
        _ => "riscv32imc-unknown-none-elf",
    }
}

fn generated_feature(feature: &str) -> Option<String> {
    // Phase 126.M4 — `nros/rmw-{zenoh,xrce,dds}-cffi` feature names were
    // dropped in Phase 128.C ("RMW-blind init + drop rmw-*-cffi features").
    // Backend selection now happens at link time via the linker-section
    // walker inside `Executor::open`, driven by the per-backend `path`
    // dep in the generated Cargo.toml. The generator collapses the old
    // per-RMW `cffi` feature aliases to plain `nros/rmw-cffi` (the only
    // C-FFI feature `nros` still exposes).
    match feature {
        "std" => Some("std".to_string()),
        "rmw-cffi"
        | "rmw-zenoh"
        | "rmw-zenoh-cffi"
        | "rmw-xrce"
        | "rmw-xrce-cffi"
        | "rmw-cyclonedds"
        | "rmw-cyclonedds-cffi" => Some("nros/rmw-cffi".to_string()),
        feature if feature.starts_with("nros/") || feature.starts_with("nros-orchestration/") => {
            Some(feature.to_string())
        }
        _ => None,
    }
}

fn uses_rmw_cffi(rmw: &str) -> bool {
    !matches!(rmw, "" | "none")
}

fn rmw_backend_feature(rmw: &str) -> Option<&'static str> {
    // Phase 126.M4 — see `generated_feature`. Per-RMW `cffi` features
    // collapsed to the single `rmw-cffi` umbrella; backend dispatch is
    // section-walker based.
    match rmw {
        "zenoh" | "rmw-zenoh" | "rmw-zenoh-cffi" => Some("rmw-cffi"),
        "xrce" | "rmw-xrce" | "rmw-xrce-cffi" => Some("rmw-cffi"),
        "cyclonedds" | "rmw-cyclonedds" | "rmw-cyclonedds-cffi" => Some("rmw-cffi"),
        "cffi" | "rmw-cffi" => None,
        "" | "none" => None,
        _ => None,
    }
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
    render_shared_state(&mut out, plan);
    out.push_str(&format!(
        "pub const CALLBACK_COUNT: usize = {callback_count};\n"
    ));
    out.push_str(&format!(
        "pub const SCHED_CONTEXT_COUNT: usize = {};\n\n",
        plan.sched_contexts.len()
    ));
    // Phase 173.5 — the locator from the first `[[transport]]` that
    // declares one. The board entry prefers it over the board
    // `Config`'s default; hosted entries keep using env (ZENOH_LOCATOR)
    // so runtime override still works. `None` ⇒ no transport locator ⇒
    // board/env default unchanged.
    let transport_locator = plan
        .build
        .transports
        .iter()
        .find_map(|t| t.locator.as_deref());
    out.push_str(&format!(
        "pub const TRANSPORT_LOCATOR: ::core::option::Option<&str> = {};\n\n",
        match transport_locator {
            Some(loc) => format!("::core::option::Option::Some({loc:?})"),
            None => "::core::option::Option::None".to_string(),
        }
    ));
    // Phase 173.5 — bridge mode (≥2 transports): one SessionSpec per
    // transport (its rmw + locator), consumed by `Executor::open_multi`.
    // Emitted only when bridging — single-transport builds use
    // `Executor::open` and never reference this.
    if plan.build.is_bridge() {
        out.push_str(&format!(
            "pub static SESSION_SPECS: [nros::SessionSpec<'static>; {}] = [\n",
            plan.build.transports.len()
        ));
        for t in &plan.build.transports {
            let rmw = t.rmw.as_deref().unwrap_or(plan.build.rmw.as_str());
            let canonical = normalize_rmw(rmw).unwrap_or(rmw);
            let locator = t.locator.as_deref().unwrap_or("");
            // Phase 172 WP-B — a transport's `domain` joins its session to a
            // distinct ROS domain (multi-domain in-binary); absent ⇒ default 0.
            let domain = match t.domain {
                Some(d) => format!(".domain_id({d})"),
                None => String::new(),
            };
            out.push_str(&format!(
                "    nros::SessionSpec::new({canonical:?}, {locator:?}){domain},\n"
            ));
        }
        out.push_str("];\n\n");
    }
    // Phase 173.5 — write the nros.toml transport IP / baud into the
    // board `Config` (NanoRosOwned). The board entry calls this on a
    // `Config::default()` before `run`, so `init_hardware` brings up the
    // NIC / UART with the configured values.
    if emits_transport_config_override(plan) {
        out.push_str("pub fn apply_transport_config<C: nros::BoardTransportConfig>(c: &mut C) {\n");
        for call in transport_config_setter_calls(&plan.build) {
            out.push_str(&call);
            out.push('\n');
        }
        out.push_str("}\n\n");
    }
    render_backend_register_fn(&mut out, plan);
    render_lifecycle_fn(&mut out, plan);
    render_param_persistence_fn(&mut out, plan);
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
    render_entry_lib_fns(&mut out, plan);
    out
}

/// Phase 172 WP-B — emit the generated entry lib's Rust-native API: the
/// `build_executor` openers and `register_all`. These are the units the
/// entry-lib C ABI wraps; the per-platform entry (and a future vendor C caller)
/// only calls them + spins.
///
/// * `build_executor(config)` — register backends + `Executor::open(config)`.
/// * `build_executor_bridge()` — backends + `Executor::open_multi(SESSION_SPECS)`
///   (emitted only in bridge mode).
/// * `register_all(executor)` — the full post-open wiring on an already-opened
///   executor: sched contexts → instantiate components → bind callbacks →
///   lifecycle → parameter persistence.
fn render_entry_lib_fns(out: &mut String, plan: &NrosPlan) {
    out.push_str(
        "\npub fn build_executor(config: &nros::ExecutorConfig<'_>) -> Result<nros::Executor, nros::NodeError> {\n",
    );
    out.push_str("    register_backends();\n");
    out.push_str("    nros::Executor::open(config)\n");
    out.push_str("}\n");
    if plan.build.is_bridge() {
        out.push_str(
            "\npub fn build_executor_bridge() -> Result<nros::Executor, nros::NodeError> {\n",
        );
        out.push_str("    register_backends();\n");
        out.push_str("    nros::Executor::open_multi(&SESSION_SPECS)\n");
        out.push_str("}\n");
    }
    render_register_all_fn(out);
}

/// Emit `register_all` (see [`render_entry_lib_fns`]).
fn render_register_all_fn(out: &mut String) {
    out.push_str(
        "\npub fn register_all(executor: &mut nros::Executor) -> Result<(), nros::NodeError> {\n",
    );
    out.push_str("    let mut callback_handles = CallbackHandleTable::<CALLBACK_COUNT>::new();\n");
    out.push_str(
        "    let mut sched_context_ids = [executor.default_sched_context_id(); SCHED_CONTEXT_COUNT + 1];\n",
    );
    out.push_str("    for (index, spec) in SCHED_CONTEXTS.iter().copied().enumerate() {\n");
    out.push_str(
        "        sched_context_ids[index + 1] = executor.create_sched_context(spec.to_nros_node())?;\n",
    );
    out.push_str("    }\n");
    out.push_str("    instantiate_components(executor, &mut callback_handles)?;\n");
    out.push_str("    for binding in CALLBACK_BINDINGS.iter().copied() {\n");
    out.push_str(
        "        let handle = callback_handles.get(binding.callback_index).ok_or(nros::NodeError::NotInitialized)?;\n",
    );
    out.push_str(
        "        let sched_context = sched_context_ids.get(binding.sched_context_index).copied().ok_or(nros::NodeError::InvalidSchedContextBinding)?;\n",
    );
    out.push_str("        executor.bind_handle_to_sched_context(handle, sched_context)?;\n");
    out.push_str("    }\n");
    out.push_str("    apply_lifecycle(executor)?;\n");
    out.push_str("    apply_param_persistence(executor)?;\n");
    out.push_str("    Ok(())\n");
    out.push_str("}\n");
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
    // Phase 173.5 — register every RMW the transports bind to (bridge
    // mode registers 2+ before `Executor::open_multi`). Single-RMW emits
    // one call, byte-identical.
    for rmw in rmw_set(&plan.build) {
        match rmw {
            "zenoh" => out.push_str("    let _ = nros_rmw_zenoh::register();\n"),
            "xrce" => out.push_str("    let _ = nros_rmw_xrce_cffi::register();\n"),
            // Cyclone DDS is a CMake/C++ project with no Rust shim;
            // registration happens through the C ABI at the CMake layer
            // (NANO_ROS_RMW=cyclonedds). No Rust call emitted.
            _ => {}
        }
    }
    out.push_str("}\n\n");
}

/// Phase 172.A — emit `apply_lifecycle`, called from `run_executor` after the
/// callbacks are bound. Unmanaged plans get a no-op (so the build needs no
/// `lifecycle-services` feature and stays byte-equivalent in behaviour); a
/// `[lifecycle]` plan registers the REP-2002 services on the executor and
/// drives the node to its boot `autostart` state.
fn render_lifecycle_fn(out: &mut String, plan: &NrosPlan) {
    out.push_str(
        "pub fn apply_lifecycle(executor: &mut nros::Executor) -> Result<(), nros::NodeError> {\n",
    );
    match &plan.lifecycle {
        None => {
            out.push_str("    let _ = executor;\n    Ok(())\n");
        }
        Some(lifecycle) => {
            out.push_str("    executor.register_lifecycle_services()?;\n");
            // Drive the boot autostart policy on the freshly-registered state
            // machine. No transition callbacks are registered, so each
            // transition takes the default-success path (REP-2002 skeleton);
            // component-provided transition hooks are a later increment.
            let transitions: &[&str] = match lifecycle.autostart {
                LifecycleAutostart::None => &[],
                LifecycleAutostart::Configure => &["Configure"],
                LifecycleAutostart::Active => &["Configure", "Activate"],
            };
            if !transitions.is_empty() {
                out.push_str("    if let Some(sm) = executor.lifecycle_state_machine_mut() {\n");
                out.push_str("        unsafe {\n");
                for t in transitions {
                    out.push_str(&format!(
                        "            let _ = sm.trigger_transition(nros::LifecycleTransition::{t});\n",
                    ));
                }
                out.push_str("        }\n    }\n");
            }
            out.push_str("    Ok(())\n");
        }
    }
    out.push_str("}\n\n");
}

/// Phase 172.H — emit `apply_param_persistence`, called from `run_executor`
/// after `apply_lifecycle`. Plans without `[param_persistence]` get a no-op (no
/// param services, byte-equivalent). A `[param_persistence]` plan registers the
/// parameter services, declares the plan's parameters as defaults, then attaches
/// the persistence backend (which overlays any persisted overrides at boot and
/// flushes runtime `set_parameters` changes from the spin loop).
fn render_param_persistence_fn(out: &mut String, plan: &NrosPlan) {
    out.push_str(
        "pub fn apply_param_persistence(executor: &mut nros::Executor) -> Result<(), nros::NodeError> {\n",
    );
    match &plan.param_persistence {
        None => {
            out.push_str("    let _ = executor;\n    Ok(())\n");
        }
        Some(pp) if pp.backend == "file" => {
            out.push_str("    executor.register_parameter_services()?;\n");
            out.push_str("    for spec in PARAMETERS.iter() {\n");
            out.push_str("        let value = match spec.value {\n");
            out.push_str("            ParameterValue::Bool(b) => nros::ParameterValue::Bool(b),\n");
            out.push_str(
                "            ParameterValue::I64(i) => nros::ParameterValue::Integer(i),\n",
            );
            out.push_str(
                "            ParameterValue::F64(f) => nros::ParameterValue::Double(f),\n",
            );
            out.push_str(
                "            ParameterValue::Str(s) => nros::ParameterValue::from_string(s).unwrap_or_default(),\n",
            );
            out.push_str("        };\n");
            out.push_str("        executor.declare_parameter(spec.name, value);\n");
            out.push_str("    }\n");
            out.push_str(&format!(
                "    executor.enable_parameter_persistence_with(nros::FileParamStore::new({:?}))?;\n",
                pp.path
            ));
            out.push_str("    Ok(())\n");
        }
        Some(pp) => {
            // Only the hosted file backend exists today; an unknown backend is
            // a config error surfaced at build time rather than silently
            // dropping persistence.
            out.push_str(&format!(
                "    let _ = executor;\n    compile_error!(\"unsupported param_persistence backend: {}\");\n    #[allow(unreachable_code)] Ok(())\n",
                pp.backend.escape_default()
            ));
        }
    }
    out.push_str("}\n\n");
}

/// Phase 172.I — emit a `static SHARED_<ID>: SharedRegion<bytes>` per
/// `nros.toml` `[[shared_state]]` region; co-located components access it as
/// `nros_generated::SHARED_<ID>`. Empty ⇒ emits nothing (byte-identical).
fn render_shared_state(out: &mut String, plan: &NrosPlan) {
    if plan.shared_state.is_empty() {
        return;
    }
    out.push_str("#[allow(unused_imports)]\nuse nros_orchestration::SharedRegion;\n");
    for region in &plan.shared_state {
        let ident: String = region
            .id
            .to_uppercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        out.push_str(&format!(
            "/// Shared region `{}` ({} bytes) — Phase 172.I.\n\
             pub static SHARED_{ident}: SharedRegion<{}> = SharedRegion::new();\n\n",
            region.id, region.bytes, region.bytes
        ));
    }
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
    if !namespace.is_empty()
        && let Some(stripped) = trimmed.strip_prefix(namespace)
    {
        let stripped = stripped.trim_matches('/');
        if !stripped.is_empty() {
            return stripped.to_string();
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

#[cfg(test)]
mod net_fragment_tests {
    use super::*;
    use crate::orchestration::plan::PlanTransport;

    fn build_with(transports: Vec<PlanTransport>) -> PlanBuildOptions {
        let mut build: PlanBuildOptions = serde_json::from_value(serde_json::json!({
            "target": "x", "board": "native", "rmw": "zenoh",
            "profile": "release", "features": [], "cfg": {}
        }))
        .unwrap();
        build.transports = transports;
        build
    }

    fn eth(ip: &str) -> PlanTransport {
        PlanTransport {
            kind: TransportKind::Ethernet,
            id: None,
            ip: Some(ip.to_string()),
            ssid: None,
            password: None,
            mac: None,
            gateway: None,
            device: None,
            baudrate: None,
            rmw: None,
            locator: None,
            domain: None,
        }
    }

    #[test]
    fn mac_and_gateway_emit_setter_calls() {
        // Phase 172.J — ethernet mac + gateway → set_mac / set_gateway.
        let mut t = eth("10.0.2.50/24");
        t.mac = Some("02:00:00:00:00:01".to_string());
        t.gateway = Some("10.0.2.2".to_string());
        let calls = transport_config_setter_calls(&build_with(vec![t]));
        assert!(
            calls
                .iter()
                .any(|c| c.contains("c.set_ipv4([10, 0, 2, 50], 24)")),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.contains("c.set_mac([0x02, 0x00, 0x00, 0x00, 0x00, 0x01])")),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.contains("c.set_gateway([10, 0, 2, 2])")),
            "{calls:?}"
        );
    }

    #[test]
    fn malformed_mac_emits_nothing() {
        // Bad mac → no set_mac (the parser returns None; ip still emits).
        let mut t = eth("10.0.2.50/24");
        t.mac = Some("zz:zz".to_string());
        let calls = transport_config_setter_calls(&build_with(vec![t]));
        assert!(!calls.iter().any(|c| c.contains("set_mac")), "{calls:?}");
        assert!(calls.iter().any(|c| c.contains("set_ipv4")), "{calls:?}");
    }

    #[test]
    fn prefix_to_netmask_converts_common_prefixes() {
        assert_eq!(prefix_to_netmask("24").as_deref(), Some("255.255.255.0"));
        assert_eq!(prefix_to_netmask("16").as_deref(), Some("255.255.0.0"));
        assert_eq!(prefix_to_netmask("8").as_deref(), Some("255.0.0.0"));
        assert_eq!(prefix_to_netmask("0").as_deref(), Some("0.0.0.0"));
        assert_eq!(prefix_to_netmask("33"), None);
    }

    #[test]
    fn ipv4_to_hex_packs_octets() {
        assert_eq!(ipv4_to_hex("10.0.2.50").as_deref(), Some("0x0a000232"));
        assert_eq!(ipv4_to_hex("255.255.255.0").as_deref(), Some("0xffffff00"));
        assert_eq!(ipv4_to_hex("10.0.2"), None);
        assert_eq!(ipv4_to_hex("10.0.2.999"), None);
    }

    #[test]
    fn zephyr_fragment_empty_without_transport() {
        assert!(zephyr_net_fragment(&build_with(vec![])).is_empty());
    }

    fn plan_with_shared_state(regions: serde_json::Value) -> NrosPlan {
        use crate::orchestration::schema::PLAN_VERSION;
        serde_json::from_value(serde_json::json!({
            "version": PLAN_VERSION,
            "system": "demo",
            "trace": {
                "system_config": "nros.toml",
                "launch_record": "r.json",
                "generated_by": "test"
            },
            "components": [],
            "instances": [],
            "interfaces": [],
            "sched_contexts": [],
            "shared_state": regions,
            "build": {
                "target": "x", "board": "native", "rmw": "zenoh",
                "profile": "release", "features": [], "cfg": {}
            }
        }))
        .expect("plan parses")
    }

    #[test]
    fn shared_state_renders_static_regions() {
        // Phase 172.I — each region → a `SharedRegion<bytes>` static, id
        // uppercased + non-alphanumeric folded to `_`.
        let plan = plan_with_shared_state(serde_json::json!([
            { "id": "blackboard", "bytes": 256 },
            { "id": "imu-cal", "bytes": 32 }
        ]));
        let mut out = String::new();
        render_shared_state(&mut out, &plan);
        assert!(
            out.contains("use nros_orchestration::SharedRegion;"),
            "{out}"
        );
        assert!(
            out.contains("pub static SHARED_BLACKBOARD: SharedRegion<256> = SharedRegion::new();"),
            "{out}"
        );
        assert!(
            out.contains("pub static SHARED_IMU_CAL: SharedRegion<32> = SharedRegion::new();"),
            "{out}"
        );
    }

    #[test]
    fn shared_state_empty_renders_nothing() {
        let mut out = String::new();
        render_shared_state(&mut out, &plan_with_shared_state(serde_json::json!([])));
        assert!(out.is_empty(), "{out}");
    }

    fn plan_with_param_persistence(pp: Option<serde_json::Value>) -> NrosPlan {
        use crate::orchestration::schema::PLAN_VERSION;
        let mut plan = serde_json::json!({
            "version": PLAN_VERSION,
            "system": "demo",
            "trace": {
                "system_config": "nros.toml",
                "launch_record": "r.json",
                "generated_by": "test"
            },
            "components": [], "instances": [], "interfaces": [], "sched_contexts": [],
            "build": {
                "target": "x86_64-unknown-linux-gnu", "board": "native", "rmw": "zenoh",
                "profile": "release", "features": [], "cfg": {}
            }
        });
        if let Some(pp) = pp {
            plan.as_object_mut()
                .unwrap()
                .insert("param_persistence".into(), pp);
        }
        serde_json::from_value(plan).expect("plan parses")
    }

    #[test]
    fn param_persistence_none_renders_noop() {
        // 172.H — no [param_persistence] ⇒ no-op fn, no param services.
        let mut out = String::new();
        render_param_persistence_fn(&mut out, &plan_with_param_persistence(None));
        assert!(out.contains("pub fn apply_param_persistence"), "{out}");
        assert!(out.contains("let _ = executor;"), "{out}");
        assert!(!out.contains("register_parameter_services"), "{out}");
        // And a None plan must not pull the param-services feature.
        let feats =
            generated_default_features(&plan_with_param_persistence(None).build, false, false);
        assert!(
            !feats.iter().any(|f| f == "nros/param-services"),
            "{feats:?}"
        );
    }

    #[test]
    fn param_persistence_file_renders_declare_and_enable() {
        // 172.H — a file backend registers services, declares params, attaches
        // the FileParamStore at the configured path, and pulls param-services.
        let plan = plan_with_param_persistence(Some(serde_json::json!({
            "backend": "file", "path": "/var/lib/nros/params.store"
        })));
        let mut out = String::new();
        render_param_persistence_fn(&mut out, &plan);
        assert!(
            out.contains("executor.register_parameter_services()?;"),
            "{out}"
        );
        assert!(
            out.contains("executor.declare_parameter(spec.name, value);"),
            "{out}"
        );
        assert!(
            out.contains(
                "executor.enable_parameter_persistence_with(nros::FileParamStore::new(\"/var/lib/nros/params.store\"))?;"
            ),
            "{out}"
        );
        assert!(out.contains("nros::ParameterValue::Integer(i)"), "{out}");

        let feats = generated_default_features(&plan.build, false, true);
        assert!(
            feats.iter().any(|f| f == "nros/param-services"),
            "{feats:?}"
        );
    }

    #[test]
    fn entry_lib_idents_and_c_abi_header() {
        // 172 WP-B — sanitizers + the directly-emitted C ABI header.
        assert_eq!(crate_ident("nros-e2e-generated"), "nros_e2e_generated");
        assert_eq!(crate_ident("a.b-c"), "a_b_c");
        let plan = plan_with_param_persistence(None); // system = "demo"
        assert_eq!(system_ident(&plan), "demo");
        let header = render_entry_header(&plan);
        assert!(
            header.contains("typedef struct NrosExecutor NrosExecutor;")
                && header.contains("} NrosConfig;"),
            "{header}"
        );
        assert!(
            header.contains("NrosExecutor *nros_demo_build_executor(const NrosConfig *cfg);")
                && header.contains("int32_t nros_demo_register_all(NrosExecutor *executor);")
                && header.contains("void nros_demo_destroy(NrosExecutor *executor);"),
            "{header}"
        );
        // Config lowering: the lib applies the param override (param > env > baked).
        let lib = render_entry_lib_rs(&plan);
        assert!(
            lib.contains("pub struct NrosConfig")
                && lib.contains("config = config.domain_id(cfg.domain_id as u32)")
                && lib.contains("config.locator = locator"),
            "{lib}"
        );
        // The std-hosted native plan emits the entry lib (lib + staticlib),
        // with the C ABI + its alloc box, and is NOT no_std.
        assert!(emits_entry_lib(&plan), "native std-hosted ⇒ entry lib");
        assert!(!lib.starts_with("#![no_std]"), "hosted lib is std:\n{lib}");
        assert!(
            lib.contains("extern crate alloc;"),
            "hosted C ABI boxes via alloc"
        );
        assert!(
            render_lib_section(&plan, "nros-e2e-generated")
                .contains("crate-type = [\"lib\", \"staticlib\"]"),
            "entry-lib crate-type"
        );
    }

    #[test]
    fn entry_lib_board_shape_is_no_std_without_c_abi() {
        // A board (no_std, no allocator) entry lib is `#![no_std]`, exposes the
        // Rust API (`register_all`), and omits the C ABI + alloc (the board
        // `self` shim calls `register_all` directly).
        let mut plan = plan_with_param_persistence(None);
        plan.build.board = "baremetal".to_string();
        plan.build.target = "thumbv7m-none-eabi".to_string();
        let lib = render_entry_lib_rs(&plan);
        assert!(
            lib.starts_with("#![no_std]\n"),
            "board lib is no_std:\n{lib}"
        );
        assert!(
            lib.contains(
                "pub use nros_generated::{SYSTEM, TRANSPORT_LOCATOR, build_executor, register_all};"
            ),
            "board lib exposes the Rust API:\n{lib}"
        );
        assert!(
            !lib.contains("extern crate alloc"),
            "no alloc on bare-metal:\n{lib}"
        );
        assert!(
            !lib.contains("pub struct NrosConfig"),
            "no C ABI on board self:\n{lib}"
        );
        assert!(
            !lib.contains("nros_demo_build_executor"),
            "no C-ABI fns:\n{lib}"
        );
    }

    #[test]
    fn session_specs_emit_per_transport_domain() {
        // 172 WP-B — a bridge's SESSION_SPECS carry each transport's domain
        // (multi-domain in-binary); a transport without `domain` stays default.
        use crate::orchestration::schema::PLAN_VERSION;
        let plan: NrosPlan = serde_json::from_value(serde_json::json!({
            "version": PLAN_VERSION, "system": "s",
            "trace": { "system_config": "nros.toml", "launch_record": "r", "generated_by": "t" },
            "components": [], "instances": [], "interfaces": [], "sched_contexts": [],
            "build": {
                "target": "x86_64-unknown-linux-gnu", "board": "native", "rmw": "zenoh",
                "profile": "release", "features": [], "cfg": {},
                "transports": [
                    { "kind": "ethernet", "rmw": "zenoh", "locator": "tcp/a:7447" },
                    { "kind": "ethernet", "rmw": "zenoh", "locator": "tcp/b:7447", "domain": 5 }
                ]
            }
        }))
        .expect("bridge plan parses");
        assert!(plan.build.is_bridge());
        let tables = render_generated_tables(&plan);
        assert!(tables.contains("pub static SESSION_SPECS"), "{tables}");
        assert!(
            tables.contains("nros::SessionSpec::new(\"zenoh\", \"tcp/a:7447\"),"),
            "default-domain transport has no .domain_id:\n{tables}"
        );
        assert!(
            tables.contains("nros::SessionSpec::new(\"zenoh\", \"tcp/b:7447\").domain_id(5)"),
            "domain-5 transport emits .domain_id(5):\n{tables}"
        );
    }

    #[test]
    fn zephyr_fragment_static_ip_and_dhcp() {
        let stat = zephyr_net_fragment(&build_with(vec![eth("10.0.2.50/24")]));
        assert!(stat.contains("CONFIG_NET_CONFIG_SETTINGS=y"));
        assert!(stat.contains("CONFIG_NET_CONFIG_MY_IPV4_ADDR=\"10.0.2.50\""));
        assert!(stat.contains("CONFIG_NET_CONFIG_MY_IPV4_NETMASK=\"255.255.255.0\""));

        let dhcp = zephyr_net_fragment(&build_with(vec![eth("dhcp")]));
        assert!(dhcp.contains("CONFIG_NET_DHCPV4=y"));
        assert!(!dhcp.contains("MY_IPV4_ADDR"));
    }
}
