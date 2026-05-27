//! Project scaffolder — promoted out of `main.rs` so `nros-cli` and any
//! other front-end can share it.
//!
//! v1 emits a colcon-compatible hello-world per `<lang> × <platform>`.
//! Use-case (`talker` / `listener` / `service` / `action`) and RMW-choice
//! diversification arrives once the `templates/` tree lands; until then
//! both fields are accepted for forward-compat but only surfaced in the
//! "Next steps" output.

use eyre::{Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct ScaffoldConfig {
    pub name: String,
    pub lang: String,
    pub platform: String,
    pub rmw: String,
    pub use_case: String,
    pub force: bool,
}

pub fn scaffold_package(cfg: &ScaffoldConfig) -> Result<()> {
    let dir = PathBuf::from(&cfg.name);
    if dir.exists() {
        if !cfg.force {
            bail!(
                "Directory '{}' already exists (use --force to overwrite)",
                cfg.name
            );
        }
        fs::remove_dir_all(&dir)?;
    }

    let build_type = format!("nros.{}.{}", cfg.lang, cfg.platform);

    fs::create_dir_all(dir.join("src"))?;

    let package_xml = format!(
        r#"<?xml version="1.0"?>
<package format="3">
  <name>{name}</name>
  <version>0.1.0</version>
  <description>{name} — nano-ros {platform} package</description>
  <maintainer email="TODO@todo.com">TODO</maintainer>
  <license>Apache-2.0</license>
  <depend>std_msgs</depend>
  <export>
    <build_type>{build_type}</build_type>
  </export>
</package>
"#,
        name = cfg.name,
        platform = cfg.platform,
    );
    fs::write(dir.join("package.xml"), package_xml)?;

    match cfg.lang.as_str() {
        "rust" => scaffold_rust(&cfg.name, &cfg.platform, &dir)?,
        "c" => scaffold_c(&cfg.name, &cfg.platform, &dir)?,
        "cpp" => scaffold_cpp(&cfg.name, &cfg.platform, &dir)?,
        other => bail!("Unknown language: {other}. Use rust, c, or cpp."),
    }

    println!("✓ Created nano-ros package '{}'", cfg.name);
    println!("  Language : {}", cfg.lang);
    println!("  Platform : {}", cfg.platform);
    println!("  RMW      : {} (template diversification: TODO)", cfg.rmw);
    println!(
        "  Use case : {} (template diversification: TODO)",
        cfg.use_case
    );
    println!("  Build    : {build_type}");
    println!();
    println!("Next steps:");
    println!("  cd {}", cfg.name);
    println!(
        "  nros build           # or: colcon build --packages-select {}",
        cfg.name
    );

    Ok(())
}

fn scaffold_rust(name: &str, platform: &str, dir: &Path) -> Result<()> {
    let mut deps = String::new();
    let is_embedded = platform != "native";

    if is_embedded {
        deps.push_str(&format!(
            "nros = {{ version = \"0.1\", default-features = false, features = [\"rmw-zenoh\", \"platform-{platform}\", \"ros-humble\"] }}\n"
        ));
        let board_crate = match platform {
            "freertos" => "nros-board-mps2-an385-freertos",
            "baremetal" => "nros-board-mps2-an385",
            "nuttx" => "nros-board-nuttx-qemu-arm",
            _ => "# TODO: add board crate for this platform",
        };
        deps.push_str(&format!("{board_crate} = {{ version = \"0.1\" }}\n"));
        deps.push_str("panic-semihosting = \"0.6\"\n");
    } else {
        deps.push_str(
            "# nros = { version = \"0.1\", features = [\"std\", \"rmw-zenoh\", \"platform-posix\", \"ros-humble\"] }\n",
        );
    }

    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[workspace]

[[bin]]
name = "{name}"
path = "src/main.rs"

[dependencies]
{deps}"#
    );
    fs::write(dir.join("Cargo.toml"), cargo_toml)?;

    let main_rs = if is_embedded {
        format!(
            r#"#![no_std]
#![no_main]

use nros::prelude::*;
// TODO: import your board crate
// use nros_board_mps2_an385_freertos::{{Config, run, println}};
use panic_semihosting as _;

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {{
    // TODO: replace with your board crate's run()
    loop {{}}
}}
"#
        )
    } else {
        format!(
            r#"fn main() {{
    println!("Hello from {name}!");
}}
"#
        )
    };
    fs::write(dir.join("src/main.rs"), main_rs)?;

    if is_embedded {
        write_default_config_toml(dir)?;
    }

    Ok(())
}

fn scaffold_c(name: &str, platform: &str, dir: &Path) -> Result<()> {
    let cmake = format!(
        r#"cmake_minimum_required(VERSION 3.16)
project({name} VERSION 0.1.0 LANGUAGES C)

set(CMAKE_C_STANDARD 11)

find_package(NanoRos REQUIRED CONFIG)

add_executable({name} src/main.c)
target_link_libraries({name} PRIVATE NanoRos::NanoRos)

install(TARGETS {name} RUNTIME DESTINATION lib/{name})
"#
    );
    fs::write(dir.join("CMakeLists.txt"), cmake)?;

    let main_c = format!(
        r#"#include <stdio.h>

int main(void) {{
    printf("Hello from {name}!\n");
    return 0;
}}
"#
    );
    fs::write(dir.join("src/main.c"), main_c)?;

    if platform != "native" {
        write_default_config_toml(dir)?;
    }
    Ok(())
}

fn scaffold_cpp(name: &str, platform: &str, dir: &Path) -> Result<()> {
    let cmake = format!(
        r#"cmake_minimum_required(VERSION 3.16)
project({name} VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 14)

find_package(NanoRos REQUIRED CONFIG)

add_executable({name} src/main.cpp)
target_link_libraries({name} PRIVATE NanoRos::NanoRosCpp)

install(TARGETS {name} RUNTIME DESTINATION lib/{name})
"#
    );
    fs::write(dir.join("CMakeLists.txt"), cmake)?;

    let main_cpp = format!(
        r#"#include <cstdio>

int main() {{
    printf("Hello from {name}!\n");
    return 0;
}}
"#
    );
    fs::write(dir.join("src/main.cpp"), main_cpp)?;

    if platform != "native" {
        write_default_config_toml(dir)?;
    }
    Ok(())
}

fn write_default_config_toml(dir: &Path) -> Result<()> {
    // Phase 172.K — scaffold the direct-mode nros.toml shape (one node + one
    // ethernet transport), not the retired config.toml.
    let nros_toml = r#"# nano-ros config (direct mode). See
# docs/design/configuration-and-transports.md.

[node]
domain_id = 0

[[transport]]
kind    = "ethernet"
ip      = "10.0.2.20/24"
mac     = "02:00:00:00:00:00"
gateway = "10.0.2.2"
locator = "tcp/10.0.2.2:7447"
"#;
    fs::write(dir.join("nros.toml"), nros_toml)?;
    Ok(())
}
