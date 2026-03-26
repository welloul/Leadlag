//! Logging module for TokioParasite.
//!
//! Sets up structured logging with the `tracing` ecosystem.
//! Supports JSON output for production and human-readable output for development.

use tracing_subscriber::{fmt, EnvFilter, prelude::*};

/// Initialize the logging system.
///
/// # Arguments
/// * `log_level` - Log level string (trace, debug, info, warn, error)
pub fn init_logging(log_level: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level));

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).with_thread_ids(true))
        .with(filter)
        .init();
}

/// Initialize logging with JSON output (for production).
pub fn init_logging_json(log_level: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level));

    tracing_subscriber::registry()
        .with(fmt::layer().json().with_target(true).with_thread_ids(true))
        .with(filter)
        .init();
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logging_init() {
        // Just verify it doesn't panic
        init_logging("info");
    }
}