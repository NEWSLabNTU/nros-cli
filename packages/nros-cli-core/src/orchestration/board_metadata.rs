//! Phase 215.C.2 — strict reader for `[package.metadata.nros.board]`.
//!
//! Each `packages/boards/nros-board-*` crate carries TWO faces of the same
//! board manifest:
//!
//! * `board.cmake` — sidecar consumed by the Zephyr cmake module
//!   `zephyr/cmake/nano_ros_use_board.cmake`.
//! * `[package.metadata.nros.board]` in `Cargo.toml` — consumed by Rust /
//!   `nros` CLI tooling (drift audit, `nros board info`).
//!
//! Both must stay in lock-step; Phase 215.F drift audit guards that
//! invariant. This module is the strict reader for the Cargo.toml face —
//! mirrors the discipline of `cargo_metadata_schema.rs` (Phase 212.B):
//! `deny_unknown_fields` so typos surface at parse time instead of being
//! silently dropped.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// `[package.metadata.nros.board]` — strict schema.
///
/// Field semantics mirror the `NROS_BOARD_*` cmake variables defined in
/// Phase 215.A.1:
///
/// | Cargo field         | `board.cmake` variable        |
/// |---------------------|-------------------------------|
/// | `zephyr_board`      | `NROS_BOARD_ZEPHYR_ID`        |
/// | `toolchain`         | `NROS_BOARD_TOOLCHAIN`        |
/// | `gated`             | `NROS_BOARD_GATED_PKGS`       |
/// | `default_rmw`       | `NROS_BOARD_DEFAULT_RMW`      |
/// | `default_transport` | `NROS_BOARD_DEFAULT_TRANSPORT`|
/// | `runner`            | `NROS_BOARD_RUNNER`           |
/// | `prj_conf`          | `NROS_BOARD_PRJ_CONF`         |
/// | `board_conf`        | `NROS_BOARD_BOARD_CONF`       |
/// | `board_overlay`     | `NROS_BOARD_BOARD_OVERLAY`    |
///
/// `prj_conf` / `board_conf` / `board_overlay` are RELATIVE to the
/// host `Cargo.toml`'s directory (the cmake face stores the absolute
/// form after resolution via the board crate dir).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardMetadata {
    /// Zephyr `BOARD` string (`fvp_baser_aemv8r/fvp_aemv8r_aarch64/smp`).
    pub zephyr_board: String,
    /// SDK abi target (e.g. `aarch64-zephyr-elf`).
    pub toolchain: String,
    /// Optional semicolon-list (in cmake) of `[features.<flag>]` gates.
    /// Defaults to empty — boards w/o gated pkgs simply omit the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gated: Vec<String>,
    /// `cyclonedds` / `zenoh` / `xrce`.
    pub default_rmw: String,
    /// `ethernet` / `serial` / …
    pub default_transport: String,
    /// `armfvp` / `qemu` / `native` / …
    pub runner: String,
    /// Relative path to `prj.conf` (relative to `Cargo.toml`).
    pub prj_conf: String,
    /// Relative path to per-board hwv2 `<board>.conf` overlay.
    pub board_conf: String,
    /// Relative path to per-board DTS overlay.
    pub board_overlay: String,
}

/// Parse `[package.metadata.nros.board]` from a board crate's `Cargo.toml`.
///
/// Strict on absent table — callers that need a fallback should handle the
/// `Err` themselves. Strict on unknown fields (`deny_unknown_fields`).
pub fn parse_board_metadata(cargo_toml: &Path) -> Result<BoardMetadata, eyre::Report> {
    let raw = std::fs::read_to_string(cargo_toml).map_err(|e| {
        eyre::eyre!(
            "failed to read {} for `[package.metadata.nros.board]`: {e}",
            cargo_toml.display()
        )
    })?;
    let value: toml::Value = toml::from_str(&raw)
        .map_err(|e| eyre::eyre!("invalid TOML in {}: {e}", cargo_toml.display()))?;
    let table = value
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("nros"))
        .and_then(|n| n.get("board"))
        .ok_or_else(|| {
            eyre::eyre!(
                "no `[package.metadata.nros.board]` table in {}",
                cargo_toml.display()
            )
        })?;
    let cloned = table.clone();
    let board: BoardMetadata = cloned.try_into().map_err(|e| {
        eyre::eyre!(
            "invalid `[package.metadata.nros.board]` in {}: {e}",
            cargo_toml.display()
        )
    })?;
    Ok(board)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const FVP_GOLDEN: &str = r#"
[package]
name = "nros-board-fvp-aemv8r-smp"
version = "0.1.0"
edition = "2024"

[package.metadata.nros.board]
zephyr_board = "fvp_baser_aemv8r/fvp_aemv8r_aarch64/smp"
toolchain    = "aarch64-zephyr-elf"
gated        = ["arm-fvp"]
default_rmw  = "cyclonedds"
default_transport = "ethernet"
runner       = "armfvp"
prj_conf      = "prj.conf"
board_conf    = "boards/fvp_baser_aemv8r_fvp_aemv8r_aarch64_smp.conf"
board_overlay = "boards/fvp_baser_aemv8r_fvp_aemv8r_aarch64_smp.overlay"
"#;

    fn write_tmp(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nros-board-metadata-test-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("Cargo.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parses_basic_metadata_table() {
        let p = write_tmp("basic", FVP_GOLDEN);
        let m = parse_board_metadata(&p).expect("golden fixture parses");
        assert_eq!(m.zephyr_board, "fvp_baser_aemv8r/fvp_aemv8r_aarch64/smp");
        assert_eq!(m.toolchain, "aarch64-zephyr-elf");
        assert_eq!(m.gated, vec!["arm-fvp"]);
        assert_eq!(m.default_rmw, "cyclonedds");
        assert_eq!(m.default_transport, "ethernet");
        assert_eq!(m.runner, "armfvp");
        assert_eq!(m.prj_conf, "prj.conf");
        assert_eq!(
            m.board_conf,
            "boards/fvp_baser_aemv8r_fvp_aemv8r_aarch64_smp.conf"
        );
        assert_eq!(
            m.board_overlay,
            "boards/fvp_baser_aemv8r_fvp_aemv8r_aarch64_smp.overlay"
        );

        // Round-trip
        let reser = toml::to_string(&m).expect("serialize");
        let m2: BoardMetadata = toml::from_str(&reser).expect("reparse");
        assert_eq!(m, m2);
    }

    #[test]
    fn omitted_gated_defaults_to_empty() {
        // Bare board w/o the `gated` knob still parses.
        let raw = r#"
[package]
name = "nros-board-bare"
version = "0.1.0"

[package.metadata.nros.board]
zephyr_board = "qemu_cortex_m3"
toolchain    = "arm-zephyr-eabi"
default_rmw  = "zenoh"
default_transport = "ethernet"
runner       = "qemu"
prj_conf      = "prj.conf"
board_conf    = "boards/qemu_cortex_m3.conf"
board_overlay = "boards/qemu_cortex_m3.overlay"
"#;
        let p = write_tmp("bare", raw);
        let m = parse_board_metadata(&p).expect("bare board parses");
        assert!(m.gated.is_empty());
    }

    #[test]
    fn rejects_unknown_field() {
        // Typo on `default_rmw` → `default_rwm`. `deny_unknown_fields`
        // surfaces the typo at parse time.
        let raw = r#"
[package]
name = "nros-board-typo"
version = "0.1.0"

[package.metadata.nros.board]
zephyr_board = "fvp_baser_aemv8r/fvp_aemv8r_aarch64/smp"
toolchain    = "aarch64-zephyr-elf"
default_rwm  = "cyclonedds"
default_transport = "ethernet"
runner       = "armfvp"
prj_conf      = "prj.conf"
board_conf    = "boards/x.conf"
board_overlay = "boards/x.overlay"
"#;
        let p = write_tmp("typo", raw);
        let err = parse_board_metadata(&p).expect_err("unknown field must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("default_rwm") || msg.contains("unknown field"),
            "diagnostic should name the typo: {msg}"
        );
    }

    #[test]
    fn rejects_missing_required_field() {
        // `runner` dropped → parse error (required field).
        let raw = r#"
[package]
name = "nros-board-incomplete"
version = "0.1.0"

[package.metadata.nros.board]
zephyr_board = "fvp_baser_aemv8r/fvp_aemv8r_aarch64/smp"
toolchain    = "aarch64-zephyr-elf"
default_rmw  = "cyclonedds"
default_transport = "ethernet"
prj_conf      = "prj.conf"
board_conf    = "boards/x.conf"
board_overlay = "boards/x.overlay"
"#;
        let p = write_tmp("missing", raw);
        let err = parse_board_metadata(&p).expect_err("missing required field must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("runner") || msg.contains("missing field"),
            "diagnostic should mention `runner`: {msg}"
        );
    }

    #[test]
    fn rejects_absent_table() {
        let raw = r#"
[package]
name = "nros-board-empty"
version = "0.1.0"
"#;
        let p = write_tmp("absent", raw);
        let err = parse_board_metadata(&p).expect_err("absent table must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("package.metadata.nros.board"),
            "diagnostic should mention the table path: {msg}"
        );
    }
}
