//! Build-time orchestration schemas.
//!
//! Schema modules are data contracts only. Planner modules consume those
//! contracts and host-side launch artifacts; generated target code remains in
//! the Phase 126.D surface.

pub mod build;
pub mod config;
pub mod generate;
pub mod manifest;
pub mod metadata_build;
pub mod names;
pub mod params;
pub mod plan;
pub mod planner;
pub mod root_config;
pub mod schema;
pub mod source_metadata;
pub mod workspace;

pub use config::{ComponentConfig, SystemConfig};
pub use plan::NrosPlan;
pub use root_config::WorkspaceConfig;
pub use source_metadata::SourceMetadata;
pub use workspace::{ComponentDeclaration, Package, Workspace};
