//! nros-codegen: Generate C or C++ bindings for ROS 2 interface files.
//!
//! Usage:
//!   nros-codegen --args-file <path> [--language c|cpp] [--verbose]
//!   nros-codegen resolve-deps --package-xml <path> --output-cmake <path> [--verbose]

use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(name = "nros-codegen")]
#[command(about = "Generate C or C++ bindings for ROS 2 interface files")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to the JSON arguments file (for default generate mode)
    #[arg(long)]
    args_file: Option<PathBuf>,

    /// Target language: "c" (default) or "cpp"
    #[arg(long, default_value = "c")]
    language: String,

    /// Verbose output
    #[arg(long)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Resolve interface dependencies from package.xml and output a CMake script
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

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Some(Command::ResolveDeps {
            package_xml,
            output_cmake,
            verbose,
        }) => {
            let config = cargo_nano_ros::ResolveDepsConfig {
                package_xml,
                output_cmake,
                verbose,
            };
            cargo_nano_ros::resolve_deps_from_package_xml(config)
        }
        None => {
            // Legacy mode: --args-file required
            let Some(args_file) = cli.args_file else {
                eprintln!("nros-codegen: --args-file is required (or use a subcommand)");
                return ExitCode::FAILURE;
            };
            match cli.language.as_str() {
                "c" => {
                    let config = cargo_nano_ros::GenerateCConfig {
                        args_file,
                        verbose: cli.verbose,
                    };
                    cargo_nano_ros::generate_c_from_args_file(config)
                }
                "cpp" => {
                    let config = cargo_nano_ros::GenerateCppConfig {
                        args_file,
                        verbose: cli.verbose,
                    };
                    cargo_nano_ros::generate_cpp_from_args_file(config)
                }
                other => {
                    eprintln!(
                        "nros-codegen: unsupported language '{other}' (expected 'c' or 'cpp')"
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nros-codegen: {e:#}");
            ExitCode::FAILURE
        }
    }
}
