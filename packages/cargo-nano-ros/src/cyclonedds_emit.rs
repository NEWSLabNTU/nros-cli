//! Phase 212.K — Cyclone DDS descriptor emit, integrated into
//! `nros generate-rust`.
//!
//! For every generated message crate, walk its `.msg` files, synthesise
//! Cyclone-shaped IDL via `nros_msg_to_idl`, shell out to the host
//! `idlc` to produce a `<pkg>_<Msg>.{c,h}` pair, drop a small
//! `register.c` translation unit + a `build.rs` that cc-compiles
//! everything into a static archive whose `__attribute__((constructor))`
//! gets pulled into the final link via `+whole-archive`.
//!
//! The whole emit is gated on:
//!   1. the host having an `idlc` binary (via `--cyclonedds-idlc`,
//!      `NROS_CYCLONEDDS_IDLC`, `which idlc`, or `build/cyclonedds/bin/idlc`)
//!   2. the consumer's Cargo manifest enabling the generated crate's
//!      `cyclonedds` feature (a no-op otherwise — `build.rs` is gated
//!      on `#[cfg(feature = "cyclonedds")]`).
//!
//! No-op when no `idlc` is found — emits a warning + leaves the
//! generated `Cargo.toml` untouched (Zenoh / XRCE consumers keep
//! building).

use eyre::{Context, Result, eyre};
use rosidl_bindgen::ament::Package;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Result of attempting a cyclonedds emit for a single generated package.
#[derive(Debug)]
pub struct EmitOutcome {
    /// Number of descriptor pairs emitted.
    pub descriptor_count: usize,
    /// `true` if the generated `Cargo.toml` should advertise the
    /// `cyclonedds` feature + carry the `build.rs`.
    pub feature_emitted: bool,
}

/// Resolve a host `idlc` path. Order of precedence:
///   1. `--cyclonedds-idlc` (caller-supplied)
///   2. `NROS_CYCLONEDDS_IDLC` env var
///   3. `<cwd>/build/cyclonedds/bin/idlc` (project-tree default)
///   4. `which idlc` on `PATH`
pub fn resolve_idlc(override_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = override_path
        && p.is_file()
    {
        return Some(p.to_path_buf());
    }
    if let Ok(s) = std::env::var("NROS_CYCLONEDDS_IDLC") {
        let p = PathBuf::from(s);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("build/cyclonedds/bin/idlc");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Ok(out) = Command::new("which").arg("idlc").output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            let p = PathBuf::from(s);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Drive the per-package emit. Returns `feature_emitted = false` when
/// the package has no messages (nothing to descriptor-ify), when no
/// `idlc` is on the host, or when emit fails (caller decides whether
/// to bail).
///
/// IDL layout (mirrors `rosidl_adapter`):
///
/// ```text
/// <pkg_output>/src/cyclonedds/
///   _idl/<pkg>/msg/<Msg>.idl    <- staged input
///   <Msg>.{c,h}                 <- idlc output
///   register.{c,h}
/// ```
///
/// We `-I <pkg_output>/src/cyclonedds/_idl` so cross-pkg `#include
/// "<other_pkg>/msg/<Other>.idl"` directives resolve as long as the
/// other pkg's IDLs got staged under the same `_idl/` root.
pub fn emit_for_package(
    package: &Package,
    package_output: &Path,
    idlc: &Path,
    verbose: bool,
) -> Result<EmitOutcome> {
    if package.interfaces.messages.is_empty() {
        return Ok(EmitOutcome {
            descriptor_count: 0,
            feature_emitted: false,
        });
    }

    let out_dir = package_output.join("src/cyclonedds");
    let idl_root = out_dir.join("_idl");
    let idl_pkg_dir = idl_root.join(&package.name).join("msg");
    fs::create_dir_all(&idl_pkg_dir)
        .with_context(|| format!("create {}", idl_pkg_dir.display()))?;

    // Also stage IDLs for every cross-pkg sibling whose generated crate
    // sits next to ours (output_dir is the parent of `package_output`).
    // Sibling pkgs already ran their own emit; we copy their staged
    // IDLs in so `-I _idl` resolves cross-pkg `#include` directives.
    if let Some(output_root) = package_output.parent() {
        for entry in fs::read_dir(output_root).into_iter().flatten() {
            let Ok(entry) = entry else { continue };
            let sibling = entry.path();
            if !sibling.is_dir() {
                continue;
            }
            let sibling_idl = sibling.join("src/cyclonedds/_idl");
            if !sibling_idl.is_dir() || sibling_idl == idl_root {
                continue;
            }
            copy_idl_tree(&sibling_idl, &idl_root)?;
        }
    }

    // 1) Synthesise + stage every IDL up front so cross-includes
    //    resolve inside `_idl/` when we run idlc.
    let mut staged: Vec<(String, PathBuf)> = Vec::new();
    for msg_name in &package.interfaces.messages {
        let msg_path = package.get_message_path(msg_name);
        let body = fs::read_to_string(&msg_path)
            .with_context(|| format!("read {}", msg_path.display()))?;
        let idl_text = nros_msg_to_idl::msg_to_idl(&body, &package.name, msg_name)
            .map_err(|e| eyre!("nros-msg-to-idl({}/{}): {e}", package.name, msg_name))?;
        let idl_path = idl_pkg_dir.join(format!("{msg_name}.idl"));
        fs::write(&idl_path, idl_text).with_context(|| format!("write {}", idl_path.display()))?;
        staged.push((msg_name.clone(), idl_path));
    }

    // 2) Drive idlc per IDL with -I <_idl-root> and emit outputs at
    //    `<out_dir>/<pkg>/msg/<Msg>.{c,h}` so cross-pkg `#include
    //    "<pkg>/msg/<Other>.h"` directives in the generated .h files
    //    resolve relative to `<out_dir>`.
    let mut descriptors: Vec<DescriptorEntry> = Vec::new();
    let pkg_out_dir = out_dir.join(&package.name).join("msg");
    fs::create_dir_all(&pkg_out_dir)
        .with_context(|| format!("create {}", pkg_out_dir.display()))?;
    for (msg_name, idl_path) in &staged {
        let status = Command::new(idlc)
            .args(["-t", "-l", "c", "-I"])
            .arg(&idl_root)
            .arg("-o")
            .arg(&pkg_out_dir)
            .arg(idl_path)
            .status()
            .with_context(|| format!("spawn idlc at {}", idlc.display()))?;
        if !status.success() {
            return Err(eyre!(
                "idlc failed on {} (status: {status})",
                idl_path.display()
            ));
        }
        let c_path = pkg_out_dir.join(format!("{msg_name}.c"));
        let h_path = pkg_out_dir.join(format!("{msg_name}.h"));
        if !c_path.is_file() || !h_path.is_file() {
            return Err(eyre!(
                "idlc did not emit {{{msg_name}.c, {msg_name}.h}} in {}",
                pkg_out_dir.display()
            ));
        }

        descriptors.push(DescriptorEntry {
            // `register.c` lives in `<out_dir>/register.c`; include
            // path is `<pkg>/msg/<Msg>.h` relative to `<out_dir>`.
            stem: format!("{}/msg/{}", package.name, msg_name),
            type_name: format!("{}::msg::dds_::{}_", package.name, msg_name),
            descriptor_symbol: format!("{}_msg_dds__{}__desc", package.name, msg_name),
        });
    }

    let crate_ident = c_ident(&package.name);
    let register_entry = format!("{crate_ident}_register_descriptors");
    fs::write(
        out_dir.join("register.h"),
        render_register_h(&register_entry),
    )
    .with_context(|| format!("write {}/register.h", out_dir.display()))?;
    fs::write(
        out_dir.join("register.c"),
        render_register_c(&register_entry, &descriptors),
    )
    .with_context(|| format!("write {}/register.c", out_dir.display()))?;

    let lib_name = format!("{crate_ident}_cyclonedds_descriptors");
    fs::write(package_output.join("build.rs"), render_build_rs(&lib_name))
        .with_context(|| format!("write {}/build.rs", package_output.display()))?;

    if verbose {
        println!(
            "    ✓ cyclonedds descriptors: {} type{}",
            descriptors.len(),
            if descriptors.len() == 1 { "" } else { "s" },
        );
    }

    Ok(EmitOutcome {
        descriptor_count: descriptors.len(),
        feature_emitted: true,
    })
}

#[derive(Debug)]
struct DescriptorEntry {
    stem: String,
    type_name: String,
    descriptor_symbol: String,
}

fn render_register_h(entry: &str) -> String {
    format!(
        "/* Auto-generated by `nros generate-rust` (Phase 212.K). */\n\
         #ifndef NROS_CYCLONEDDS_DESCRIPTORS_REGISTER_H\n\
         #define NROS_CYCLONEDDS_DESCRIPTORS_REGISTER_H\n\
         #ifdef __cplusplus\nextern \"C\" {{\n#endif\n\
         void {entry}(void);\n\
         #ifdef __cplusplus\n}}\n#endif\n\
         #endif\n"
    )
}

fn render_register_c(entry: &str, descriptors: &[DescriptorEntry]) -> String {
    let mut s = String::new();
    s.push_str("/* Auto-generated by `nros generate-rust` (Phase 212.K). */\n");
    s.push_str("#include \"dds/dds.h\"\n");
    for d in descriptors {
        s.push_str(&format!("#include \"{}.h\"\n", d.stem));
    }
    s.push('\n');
    for d in descriptors {
        s.push_str(&format!(
            "extern const dds_topic_descriptor_t {};\n",
            d.descriptor_symbol
        ));
    }
    s.push('\n');
    s.push_str(
        "extern void nros_rmw_cyclonedds_register_descriptor(\n    \
         const char *type_name, const dds_topic_descriptor_t *desc);\n\n",
    );
    s.push_str(&format!("void {entry}(void) {{\n"));
    for d in descriptors {
        s.push_str(&format!(
            "    nros_rmw_cyclonedds_register_descriptor(\n        \"{}\",\n        &{});\n",
            d.type_name, d.descriptor_symbol
        ));
    }
    s.push_str("}\n\n");
    s.push_str(&format!(
        "__attribute__((constructor))\nstatic void {entry}_ctor(void) {{\n    {entry}();\n}}\n"
    ));
    s
}

fn render_build_rs(lib_name: &str) -> String {
    format!(
        r#"//! Auto-generated by `nros generate-rust` (Phase 212.K).
//!
//! Compiles the Cyclone DDS descriptor C sources emitted under
//! `src/cyclonedds/` into a static archive that the consumer link
//! pulls in with `+whole-archive`, so the per-type
//! `__attribute__((constructor))` register hook survives.
//!
//! Also exports `cargo:include=<src>` so downstream generated crates
//! (e.g. `std_msgs` depending on `builtin_interfaces`) can `#include
//! "<other_pkg>/msg/<Other>.h"` from the upstream crate's emit.
//!
//! No-op without the `cyclonedds` Cargo feature.

fn main() {{
    #[cfg(feature = "cyclonedds")]
    emit();
}}

#[cfg(feature = "cyclonedds")]
fn emit() {{
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let src = std::path::PathBuf::from(&manifest_dir).join("src/cyclonedds");
    let ddsc_include = std::env::var("DEP_DDSC_INCLUDE")
        .expect("DEP_DDSC_INCLUDE not set — enable the `cyclonedds` feature \
                 on a Cargo manifest that depends on `cyclonedds-sys`.");

    let mut cc = cc::Build::new();
    cc.include(&src).include(&ddsc_include);
    // Pick up include dirs from every upstream generated crate that
    // exported `cargo:include=<path>` via its own `links =
    // "*_cyclonedds_descriptors"` key.
    for (k, v) in std::env::vars() {{
        if k.starts_with("DEP_") && k.ends_with("_CYCLONEDDS_DESCRIPTORS_INCLUDE") {{
            cc.include(&v);
        }}
    }}
    walk_cs(&src, &mut cc);
    cc.cargo_metadata(false);
    cc.compile("{lib_name}");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    println!("cargo:rustc-link-search=native={{out_dir}}");
    println!(
        "cargo:rustc-link-lib=static:+whole-archive,-bundle={lib_name}"
    );
    println!("cargo:include={{}}", src.display());
}}

#[cfg(feature = "cyclonedds")]
fn walk_cs(dir: &std::path::Path, cc: &mut cc::Build) {{
    // Skip the IDL staging tree — `_idl/` only carries .idl sources.
    if dir.file_name().and_then(|n| n.to_str()) == Some("_idl") {{
        return;
    }}
    for entry in std::fs::read_dir(dir).expect("read_dir cyclonedds") {{
        let entry = entry.expect("dirent");
        let path = entry.path();
        if path.is_dir() {{
            walk_cs(&path, cc);
        }} else if path.extension().and_then(|e| e.to_str()) == Some("c") {{
            cc.file(&path);
            println!("cargo:rerun-if-changed={{}}", path.display());
        }}
    }}
}}
"#
    )
}

/// Copy every `.idl` from `src` into `dst`, preserving the relative
/// tree. Used to flatten sibling-pkg IDLs into our own `_idl/` so
/// `idlc -I` resolves cross-pkg includes.
fn copy_idl_tree(src: &Path, dst: &Path) -> Result<()> {
    copy_idl_tree_inner(src, src, dst)
}

fn copy_idl_tree_inner(root: &Path, cur: &Path, dst: &Path) -> Result<()> {
    for entry in fs::read_dir(cur).with_context(|| format!("read_dir {}", cur.display()))? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .expect("strip_prefix must succeed under root");
        let target = dst.join(rel);
        if path.is_dir() {
            fs::create_dir_all(&target).with_context(|| format!("mkdir {}", target.display()))?;
            copy_idl_tree_inner(root, &path, dst)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("idl") {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("mkdir {}", parent.display()))?;
            }
            fs::copy(&path, &target)
                .with_context(|| format!("copy {} -> {}", path.display(), target.display()))?;
        }
    }
    Ok(())
}

fn c_ident(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Patch a freshly-emitted `<pkg>/Cargo.toml` to advertise the
/// `cyclonedds` feature + the matching `[build-dependencies]` /
/// `[dependencies]` entries, and inject a `links` key so cargo
/// propagates the build.rs `cargo:include=` metadata to downstream
/// generated crates (`DEP_<PKG>_CYCLONEDDS_DESCRIPTORS_INCLUDE`).
///
/// Idempotent — running again over the same file is a no-op (the
/// appended section is keyed off a marker comment).
pub fn patch_cargo_toml_with_cyclonedds_feature(
    cargo_path: &Path,
    package_name: &str,
) -> Result<()> {
    let mut body =
        fs::read_to_string(cargo_path).with_context(|| format!("read {}", cargo_path.display()))?;
    let marker = "# === nros-managed cyclonedds feature ===";
    if body.contains(marker) {
        return Ok(());
    }

    // Inject `links = "<pkg>_cyclonedds_descriptors"` under `[package]`
    // so cargo propagates the build.rs `cargo:include=...` line as
    // `DEP_<PKG>_CYCLONEDDS_DESCRIPTORS_INCLUDE` to downstream crates.
    let links_value = format!("{}_cyclonedds_descriptors", c_ident(package_name));
    if !body.contains("links =") {
        if let Some(pos) = body.find("[package]\n") {
            // After the `version = "..."` line — find the blank
            // line that closes the [package] table.
            let after_header = pos + "[package]\n".len();
            let rest = &body[after_header..];
            // Insert directly after the header so it sits at the
            // top of the table (cargo doesn't care about ordering).
            let insert_at = after_header + rest.len() - rest.trim_start().len();
            body.insert_str(insert_at, &format!("links = \"{links_value}\"\n"));
        }
    }

    // Discover sibling generated-crate deps (path = "../<dep>") and
    // forward the `cyclonedds` feature transitively so a downstream
    // consumer enabling `std_msgs/cyclonedds` also flips the same
    // feature on `builtin_interfaces`.
    let mut forwards: Vec<String> = Vec::new();
    for line in body.lines() {
        // shape: `<crate> = { path = "../<dep>", default-features = false }`
        let trim = line.trim_start();
        if let Some(eq) = trim.find('=') {
            let lhs = trim[..eq].trim();
            let rhs = trim[eq + 1..].trim();
            if rhs.starts_with("{ path = \"../") && !lhs.is_empty() {
                forwards.push(lhs.to_string());
            }
        }
    }

    // Splice `cyclonedds = [...]` into the existing `[features]`
    // block. The generator always emits a `[features]` block with
    // a `default = []` row.
    let mut feature_parts: Vec<String> = vec![
        "\"dep:cc\"".to_string(),
        "\"dep:cyclonedds-sys\"".to_string(),
    ];
    for dep in &forwards {
        feature_parts.push(format!("\"{dep}/cyclonedds\""));
    }
    let injected_feature = format!("cyclonedds = [{}]\n", feature_parts.join(", "));
    if let Some(pos) = body.find("[features]\n") {
        // Insert after the `default = ...` line that follows the
        // `[features]` header.
        let after_header = pos + "[features]\n".len();
        let rest = &body[after_header..];
        let line_end = rest.find('\n').map(|n| n + 1).unwrap_or(rest.len());
        let insert_at = after_header + line_end;
        body.insert_str(insert_at, &injected_feature);
    } else {
        // Defensive: append a fresh [features] block.
        body.push_str("\n[features]\n");
        body.push_str(&injected_feature);
    }

    body.push('\n');
    body.push_str(marker);
    body.push('\n');
    body.push_str(
        "[build-dependencies]\n\
         cc = { version = \"1.0\", optional = true }\n\n\
         # `cyclonedds-sys` MUST sit in `[dependencies]` (NOT\n\
         # `[build-dependencies]`) so cargo propagates the\n\
         # `links = \"ddsc\"` metadata (`DEP_DDSC_INCLUDE` etc.) into\n\
         # this crate's `build.rs`.\n",
    );
    body.push_str(
        "[dependencies.cyclonedds-sys]\n\
         version = \"*\"\n\
         optional = true\n",
    );

    fs::write(cargo_path, body).with_context(|| format!("write {}", cargo_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_ident_sanitises() {
        assert_eq!(c_ident("std_msgs"), "std_msgs");
        assert_eq!(c_ident("foo-bar"), "foo_bar");
        assert_eq!(c_ident("9live"), "_9live");
    }

    #[test]
    fn render_register_c_contains_constructor_hook() {
        let descs = vec![DescriptorEntry {
            stem: "std_msgs/msg/Int32".into(),
            type_name: "std_msgs::msg::dds_::Int32_".into(),
            descriptor_symbol: "std_msgs_msg_dds__Int32__desc".into(),
        }];
        let body = render_register_c("std_msgs_register_descriptors", &descs);
        assert!(body.contains("__attribute__((constructor))"));
        assert!(body.contains("&std_msgs_msg_dds__Int32__desc"));
        assert!(body.contains("\"std_msgs::msg::dds_::Int32_\""));
        assert!(body.contains("#include \"std_msgs/msg/Int32.h\""));
    }

    #[test]
    fn patch_cargo_toml_injects_feature_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        fs::write(
            &cargo,
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [features]\ndefault = []\nstd = []\n\n\
             [dependencies]\nheapless = \"0.8\"\n",
        )
        .unwrap();
        patch_cargo_toml_with_cyclonedds_feature(&cargo, "foo").unwrap();
        let body = fs::read_to_string(&cargo).unwrap();
        assert!(body.contains("cyclonedds = [\"dep:cc\", \"dep:cyclonedds-sys\"]"));
        assert!(body.contains("[build-dependencies]"));
        assert!(body.contains("[dependencies.cyclonedds-sys]"));
        assert!(body.contains("links = \"foo_cyclonedds_descriptors\""));
        // Idempotent.
        patch_cargo_toml_with_cyclonedds_feature(&cargo, "foo").unwrap();
        let body2 = fs::read_to_string(&cargo).unwrap();
        assert_eq!(body, body2);
    }
}
