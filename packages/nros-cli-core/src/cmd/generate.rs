//! `nros generate <lang>` — message bindings codegen.
//!
//! Phase 111.A.5. Wraps the existing `cargo_nano_ros` library API
//! one-for-one so output is byte-identical to `cargo nano-ros
//! generate-{rust,c,cpp}`.

use cargo_nano_ros::{
    GenerateCStandaloneConfig, GenerateConfig, generate_c_from_package_xml,
    generate_from_package_xml,
};
use clap::{Args as ClapArgs, ValueEnum};
use eyre::{Result, eyre};
use std::{collections::HashMap, path::PathBuf};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Lang {
    Rust,
    C,
    Cpp,
    /// Generate Rust + C + C++ in one shot
    All,
}

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Target language
    #[arg(value_enum)]
    pub lang: Lang,

    /// Path to `package.xml`
    #[arg(long, default_value = "package.xml")]
    pub manifest: PathBuf,

    /// Output directory for generated bindings
    #[arg(long, default_value = "generated")]
    pub output: PathBuf,

    /// ROS 2 edition (`humble` | `iron`)
    #[arg(long, default_value = "humble")]
    pub ros_edition: String,

    /// Overwrite existing bindings
    #[arg(long)]
    pub force: bool,

    /// Verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// Generate `.cargo/config.toml` with `[patch.crates-io]` entries
    /// (Rust only)
    #[arg(long)]
    pub generate_config: bool,
}

pub fn run(args: Args) -> Result<()> {
    match args.lang {
        Lang::Rust => generate_rust(&args),
        Lang::C => generate_c(&args),
        Lang::Cpp => Err(eyre!(
            "`nros generate cpp` standalone mode is not yet wired up. \
             Use the CMake `nano_ros_generate_interfaces(... LANGUAGE CPP)` \
             integration for C++ codegen."
        )),
        Lang::All => {
            generate_rust(&args)?;
            generate_c(&args)?;
            // C++ standalone path missing — see comment above.
            Ok(())
        }
    }
}

fn generate_rust(args: &Args) -> Result<()> {
    let cfg = GenerateConfig {
        manifest_path: args.manifest.clone(),
        output_dir: args.output.clone(),
        generate_config: args.generate_config,
        nano_ros_path: None,
        nano_ros_git: false,
        force: args.force,
        verbose: args.verbose,
        ros_edition: args.ros_edition.clone(),
        renames: HashMap::new(),
    };
    generate_from_package_xml(cfg)
}

fn generate_c(args: &Args) -> Result<()> {
    let cfg = GenerateCStandaloneConfig {
        manifest_path: args.manifest.clone(),
        output_dir: args.output.clone(),
        force: args.force,
        verbose: args.verbose,
        ros_edition: args.ros_edition.clone(),
    };
    generate_c_from_package_xml(cfg)
}
