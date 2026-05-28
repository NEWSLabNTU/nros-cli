//! Phase 187.1 — the SDK package index that `nros setup` reads.
//!
//! `nros-sdk-index.toml` is the versioned manifest of host toolchains/tools.
//! Each `[tool.*]` carries a per-host prebuilt `dist` (GitHub Release asset URL
//! + sha256) **and** a `[tool.*.source]` recipe used when no `dist` matches the
//! host — both install into the same `$NROS_HOME/sdk/<tool>/<version>/` prefix.
//! `[source.*]` packages build with the app (target-compiled, never prebuilt);
//! `[gated.*]` are license-gated (never fetched/built — instruct + env check).
//!
//! This module is the format + loader (the rest of `nros setup` — board
//! resolution, fetch/cache, the CI release gate — is Phase 187.2–187.5). See
//! `docs/design/nros-setup-toolchain-management.md`.

use std::{collections::BTreeMap, path::Path};

use eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

/// The whole `nros-sdk-index.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdkIndex {
    /// Prebuilt host tools (qemu, cross-gcc, zenohd, …), keyed by tool name.
    #[serde(default)]
    pub tool: BTreeMap<String, ToolPackage>,
    /// Source packages built with the app (kernels, small C libs), by name.
    #[serde(default)]
    pub source: BTreeMap<String, SourcePackage>,
    /// License-gated packages (never hosted/built), by name.
    #[serde(default)]
    pub gated: BTreeMap<String, GatedPackage>,
}

/// A prebuilt host tool: a per-host `dist` map + an optional `source` fallback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPackage {
    pub version: String,
    /// host key (`<os>-<arch>`, e.g. `linux-x86_64`) → prebuilt artifact.
    #[serde(default)]
    pub dist: BTreeMap<String, DistArtifact>,
    /// Build-from-source recipe used when no `dist` matches the host.
    #[serde(default)]
    pub source: Option<ToolSource>,
}

/// A prebuilt artifact for one host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistArtifact {
    pub url: String,
    pub sha256: String,
}

/// The source-build fallback recipe — installs into the same prefix as `dist`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSource {
    pub git: String,
    /// Git ref (tag/sha) — pinned in lockstep with the prebuilt `version`.
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Configure step; `{prefix}` is substituted with the install prefix.
    #[serde(default)]
    pub configure: Option<String>,
    /// Build + install step.
    #[serde(default)]
    pub install: Option<String>,
}

/// A package compiled with the user's app for their chosen target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePackage {
    pub version: String,
}

/// A license-gated package: never fetched or built; `nros setup` instructs the
/// user and `nros doctor` checks the `env` var points at the installed SDK.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatedPackage {
    pub version: String,
    pub env: String,
    #[serde(default)]
    pub installer: Option<String>,
}

impl SdkIndex {
    /// Read + parse an `nros-sdk-index.toml`.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read SDK index {}", path.display()))?;
        Self::parse(&raw).wrap_err_with(|| format!("invalid SDK index {}", path.display()))
    }

    /// Parse from a string.
    pub fn parse(raw: &str) -> Result<Self> {
        toml::from_str(raw).wrap_err("invalid nros-sdk-index.toml schema")
    }
}

impl ToolPackage {
    /// The prebuilt artifact for `host` (e.g. `linux-x86_64`), if one exists.
    pub fn dist_for(&self, host: &str) -> Option<&DistArtifact> {
        self.dist.get(host)
    }

    /// Whether this tool can be installed on `host` — a matching prebuilt, or a
    /// source recipe to fall back to. (`false` ⇒ no prebuilt + no source.)
    pub fn installable_on(&self, host: &str) -> bool {
        self.dist.contains_key(host) || self.source.is_some()
    }
}

/// The current host key (`<os>-<arch>`), matching `dist` map keys.
pub fn host_key() -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other, // x86_64, riscv64, …
    };
    format!("{}-{arch}", std::env::consts::OS) // linux / macos / windows
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[tool.qemu]
version = "11.0-nros1"
dist.linux-x86_64 = { url = "https://github.com/org/nano-ros-sdk/releases/download/qemu-11.0-nros1/qemu-linux-x86_64.tar.zst", sha256 = "aa" }
dist.macos-arm64  = { url = "https://example/qemu-macos-arm64.tar.zst", sha256 = "bb" }
[tool.qemu.source]
git = "https://github.com/org/qemu"
ref = "v11.0-nros1"
configure = "./configure --prefix={prefix} --target-list=arm-softmmu"
install = "make -j && make install"

[tool.arm-none-eabi-gcc]
version = "13.2"
dist.linux-x86_64 = { url = "https://example/arm-gcc-linux-x86_64.tar.zst", sha256 = "cc" }

[source.freertos-kernel]
version = "10.6.2"

[gated.nv-spe-fsp]
version = "36.3"
env = "NV_SPE_FSP_DIR"
installer = "nvidia-sdk-manager"
"#;

    #[test]
    fn parses_tool_source_and_gated_sections() {
        let idx = SdkIndex::parse(SAMPLE).expect("sample parses");
        assert_eq!(idx.tool.len(), 2);
        assert_eq!(idx.source.len(), 1);
        assert_eq!(idx.gated.len(), 1);

        let qemu = &idx.tool["qemu"];
        assert_eq!(qemu.version, "11.0-nros1");
        assert_eq!(qemu.dist_for("linux-x86_64").unwrap().sha256, "aa");
        assert!(qemu.dist_for("windows-x86_64").is_none());
        let src = qemu.source.as_ref().expect("qemu has a source recipe");
        assert_eq!(src.git_ref, "v11.0-nros1"); // the `ref` key
        assert!(src.configure.as_deref().unwrap().contains("{prefix}"));

        assert_eq!(idx.source["freertos-kernel"].version, "10.6.2");
        assert_eq!(idx.gated["nv-spe-fsp"].env, "NV_SPE_FSP_DIR");
    }

    #[test]
    fn installable_on_uses_dist_or_source_fallback() {
        let idx = SdkIndex::parse(SAMPLE).unwrap();
        // qemu: prebuilt for linux, source fallback covers any host.
        assert!(idx.tool["qemu"].installable_on("linux-x86_64"));
        assert!(idx.tool["qemu"].installable_on("freebsd-riscv64")); // via source
        // arm-gcc: prebuilt only for linux-x86_64, no source → not installable elsewhere.
        assert!(idx.tool["arm-none-eabi-gcc"].installable_on("linux-x86_64"));
        assert!(!idx.tool["arm-none-eabi-gcc"].installable_on("macos-arm64"));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let bad = "[tool.qemu]\nversion = \"1\"\nbogus = true\n";
        assert!(SdkIndex::parse(bad).is_err());
    }

    #[test]
    fn host_key_is_os_dash_arch() {
        let k = host_key();
        assert!(k.contains('-'), "host key looks like <os>-<arch>: {k}");
        assert!(!k.contains("aarch64"), "arch normalized to arm64: {k}");
    }
}
