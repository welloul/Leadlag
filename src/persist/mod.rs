//! Persistence module for TokioParasite.
//!
//! Implements:
//! - Telemetry writer (Proto3 binary files)
//! - State store (Sled embedded database)

pub mod telemetry;
pub mod state;

pub use telemetry::TelemetryWriter;
pub use state::StateStore;

use crate::config::StorageSettings;

/// Initialize storage directories.
pub fn init_storage(settings: &StorageSettings) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(&settings.telemetry_path)?;
    std::fs::create_dir_all(&settings.state_db_path)?;
    Ok(())
}