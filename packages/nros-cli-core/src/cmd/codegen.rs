//! `nros codegen` — build-tool-facing C/C++ binding generation.
//!
//! Phase 195.A: folds the former standalone `nros-codegen` binary
//! (`nros-codegen-c`) into the canonical `nros` CLI. Same engine
//! (`cargo_nano_ros`), same call shape, so the cmake / build.rs consumers
//! only change the program name (`nros-codegen …` → `nros codegen …`):
//!
//!   nros codegen --args-file <path> [--language c|cpp] [--verbose]
//!   nros codegen resolve-deps --package-xml <path> --output-cmake <path> [--verbose]
//!
//! Distinct from `nros generate` (the user-facing, `package.xml`-driven surface):
//! this is the JSON-`--args-file` contract the build system already speaks.

use clap::{Args as ClapArgs, Subcommand};
use eyre::{Result, bail, eyre};
use std::path::PathBuf;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Sub>,

    /// Path to the JSON arguments file (default generate mode)
    #[arg(long)]
    pub args_file: Option<PathBuf>,

    /// Target language: "c" (default) or "cpp"
    #[arg(long, default_value = "c")]
    pub language: String,

    /// Verbose output
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Debug, Subcommand)]
pub enum Sub {
    /// Resolve interface dependencies from package.xml → a CMake script
    ResolveDeps {
        /// Path to package.xml
        #[arg(long)]
        package_xml: PathBuf,

        /// Path to output .cmake file
        #[arg(long)]
        output_cmake: PathBuf,

        /// Verbose output
        #[arg(long)]
        verbose: bool,
    },
}

pub fn run(args: Args) -> Result<()> {
    match args.command {
        Some(Sub::ResolveDeps {
            package_xml,
            output_cmake,
            verbose,
        }) => cargo_nano_ros::resolve_deps_from_package_xml(cargo_nano_ros::ResolveDepsConfig {
            package_xml,
            output_cmake,
            verbose,
        })
        .map_err(|e| eyre!("{e:#}")),
        None => {
            let Some(args_file) = args.args_file else {
                bail!("nros codegen: --args-file is required (or use a subcommand)");
            };
            match args.language.as_str() {
                "c" => cargo_nano_ros::generate_c_from_args_file(cargo_nano_ros::GenerateCConfig {
                    args_file,
                    verbose: args.verbose,
                })
                .map_err(|e| eyre!("{e:#}")),
                "cpp" => {
                    cargo_nano_ros::generate_cpp_from_args_file(cargo_nano_ros::GenerateCppConfig {
                        args_file,
                        verbose: args.verbose,
                    })
                    .map_err(|e| eyre!("{e:#}"))
                }
                other => {
                    bail!("nros codegen: unsupported language '{other}' (expected 'c' or 'cpp')")
                }
            }
        }
    }
}
