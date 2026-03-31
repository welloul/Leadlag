//! Settings schema for TokioParasite configuration.
//!
//! All settings are validated at startup using the `validator` crate.
//! Environment variable expansion is supported for secrets (e.g., `${BINANCE_API_KEY}`).

use serde::Deserialize;
use validator::Validate;

// ============================================================================
// Top-Level Settings
// ============================================================================

/// Root configuration for the TokioParasite bot.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub app: AppSettings,
    pub storage: StorageSettings,
    pub venues: VenuesSettings,
    pub strategy: StrategySettings,
    pub risk: RiskSettings,
    pub simulation: SimulationSettings,
}

impl Settings {
    /// Load and validate settings from `settings.toml` and environment variables.
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let config = config::Config::builder()
            .add_source(config::File::with_name("settings"))
            .add_source(config::Environment::with_prefix("APP").separator("__"))
            .build()?;

        let settings: Settings = config.try_deserialize()?;

        // Semantic validation
        settings.validate()?;

        // Custom power-of-two check for ring buffer
        if !settings.strategy.window_size_ticks.is_power_of_two() {
            return Err(format!(
                "strategy.window_size_ticks must be a power of two, got {}",
                settings.strategy.window_size_ticks
            )
            .into());
        }

        Ok(settings)
    }
}

// ============================================================================
// App Settings
// ============================================================================

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    /// Log level: trace, debug, info, warn, error
    #[validate(length(min = 1))]
    pub log_level: String,

    /// Enable performance optimizations (CPU pinning, spin-loop)
    pub perf_mode: bool,

    /// Enable CPU pinning for hot path thread
    pub cpu_pinning: bool,

    /// Time-grid precision in nanoseconds (e.g., 5_000_000 for 5ms)
    #[validate(range(min = 100_000, max = 100_000_000))]
    pub tick_precision_ns: u64,
}

// ============================================================================
// Storage Settings
// ============================================================================

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct StorageSettings {
    /// Path for Proto3 telemetry binary files
    #[validate(length(min = 1))]
    pub telemetry_path: String,

    /// Path for Sled state database
    #[validate(length(min = 1))]
    pub state_db_path: String,
}

// ============================================================================
// Venue Settings
// ============================================================================

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct VenuesSettings {
    pub exchange_a: VenueConfig,
    pub exchange_b: VenueConfig,
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct VenueConfig {
    /// Exchange name (e.g., "binance_futures", "hyperliquid")
    #[validate(length(min = 1))]
    pub name: String,

    /// API key (supports ${ENV_VAR} expansion)
    #[validate(length(min = 1))]
    pub api_key: String,

    /// API secret (supports ${ENV_VAR} expansion)
    #[validate(length(min = 1))]
    pub api_secret: String,

    /// WebSocket URL
    #[validate(url)]
    pub ws_url: String,

    /// REST API URL
    #[validate(url)]
    pub rest_url: String,

    /// Maximum round-trip time in milliseconds before kill switch
    #[validate(range(min = 10, max = 1000))]
    pub max_rtt_ms: u64,
}

// ============================================================================
// Strategy Settings
// ============================================================================

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct StrategySettings {
    /// Active strategy: "correlation_hysteresis" or "impulse_obi"
    #[validate(length(min = 1))]
    pub active_strategy: String,

    /// Symbols to trade (e.g., ["BTC", "ETH", "SOL"])
    #[validate(length(min = 1))]
    pub symbols: Vec<String>,

    // --- Correlation-Hysteresis settings ---
    /// Number of ticks in the sliding window (must be power of 2)
    #[validate(range(min = 16, max = 4096))]
    pub window_size_ticks: usize,

    /// Minimum Pearson correlation R to trigger a signal
    #[validate(range(min = 0.5, max = 1.0))]
    pub min_correlation_r: f64,

    /// Hysteresis buffer for role-flip stability (0.0 to 1.0)
    #[validate(range(min = 0.01, max = 0.5))]
    pub hysteresis_buffer: f64,

    /// Enable Order Book Imbalance fusion
    pub enable_obi: bool,

    /// Weight of OBI vs Trade Delta (0.0 = only delta, 1.0 = only OBI)
    #[validate(range(min = 0.0, max = 1.0))]
    pub obi_weight: f64,

    // --- Impulse-OBI settings ---
    /// Price move threshold in bps to detect impulse (3-10 bps)
    #[validate(range(min = 1, max = 100))]
    pub impulse_threshold_bps: u64,

    /// Max move on other exchange to consider it "lagging" (0.5-5 bps can be fractional)
    #[validate(range(min = 0.1, max = 50.0))]
    pub lag_threshold_bps: f64,

    /// Lookback window in ms for price change detection
    #[validate(range(min = 1, max = 100))]
    pub impulse_window_ms: u64,

    /// Signal timeout in ms (cancel if not filled)
    #[validate(range(min = 1, max = 1000))]
    pub signal_timeout_ms: u64,

    /// Minimum trade size to filter fake impulses
    #[validate(range(min = 0.0, max = 1.0))]
    pub min_trade_size_filter: f64,

    /// Maximum spread in bps to allow trading
    #[validate(range(min = 1, max = 1000))]
    pub spread_filter_bps: u64,

    /// OBI strong threshold (0.6-0.8)
    #[validate(range(min = 0.5, max = 1.0))]
    pub obi_strong_threshold: f64,

    /// OBI neutral threshold
    #[validate(range(min = 0.0, max = 0.5))]
    pub obi_neutral_threshold: f64,

    /// Order book depth for OBI calculation
    #[validate(range(min = 1, max = 100))]
    pub obi_depth: usize,

    /// OBI spike threshold for liquidity shift detection
    #[validate(range(min = 0.01, max = 1.0))]
    pub obi_spike_threshold: f64,

    // --- Entry logic tightening (v0.1.3) ---
    /// Venue freshness threshold in ms — both venues must have ticked within this window
    #[validate(range(min = 50, max = 2000))]
    pub venue_freshness_ms: u64,

    /// Minimum cross-venue edge in bps to enter a trade (should cover fees + slippage)
    #[validate(range(min = 2, max = 50))]
    pub entry_threshold_bps: u64,

    /// Cooldown in ms between trades for same (symbol, side) pair
    #[validate(range(min = 5, max = 5000))]
    pub cooldown_ms: u64,

    /// Maximum number of price levels to consume per fill
    #[validate(range(min = 1, max = 10))]
    pub max_levels_consumed: usize,

    /// OBI persistence duration in ms — OBI must stay strong for this long
    #[validate(range(min = 50, max = 2000))]
    pub obi_persist_ms: u64,

    /// Fill conservatism — fraction of best level size to allow (0.5 = 50%)
    #[validate(range(min = 0.1, max = 1.0))]
    pub fill_conservatism: f64,

    /// Only trade high-conviction signals
    pub high_conviction_only: bool,

    /// Time-based exit timeout in ms — close positions older than this
    #[validate(range(min = 100, max = 60000))]
    pub exit_timeout_ms: u64,
}

// ============================================================================
// Risk Settings
// ============================================================================

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct RiskSettings {
    /// Maximum notional USD per single trade
    #[validate(range(min = 1.0, max = 1_000_000.0))]
    pub max_notional_usd: f64,

    /// Maximum daily drawdown in USD before circuit breaker
    #[validate(range(min = 1.0, max = 100_000.0))]
    pub max_drawdown_daily: f64,

    /// Maximum slippage in basis points (e.g., 8 = 0.08%)
    #[validate(range(min = 1, max = 100))]
    pub max_slippage_bps: u64,

    /// Signal time-to-live in milliseconds
    #[validate(range(min = 5, max = 5000))]
    pub signal_ttl_ms: u64,

    /// Enable self-trade prevention
    pub self_trade_prevention: bool,
}

// ============================================================================
// Simulation Settings
// ============================================================================

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct SimulationSettings {
    /// Enable paper trading mode
    pub enabled: bool,

    /// Use real market data feeds (Binance, Hyperliquid)
    /// No API keys required for market data
    pub use_real_data: bool,

    /// Artificial round-trip time in milliseconds
    #[validate(range(min = 0, max = 1000))]
    pub latency_simulation_ms: u64,

    /// Fee tier in basis points (e.g., 2.5 = 0.025%)
    #[validate(range(min = 0.0, max = 50.0))]
    pub fee_tier_bps: f64,

    /// Number of L2 order book levels to match against
    #[validate(range(min = 1, max = 100))]
    pub match_l2_depth: usize,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings_load() {
        let settings = Settings::load().expect("Failed to load settings");
        assert_eq!(settings.app.log_level, "info");
        assert!(settings.app.perf_mode);
        assert!(settings.app.cpu_pinning);
        assert_eq!(settings.strategy.window_size_ticks, 256);
        assert!(settings.strategy.window_size_ticks.is_power_of_two());
    }

    #[test]
    fn test_venue_config_symmetry() {
        let settings = Settings::load().expect("Failed to load settings");
        // Both venues should have the same schema
        assert!(!settings.venues.exchange_a.name.is_empty());
        assert!(!settings.venues.exchange_b.name.is_empty());
        assert!(!settings.venues.exchange_a.ws_url.is_empty());
        assert!(!settings.venues.exchange_b.ws_url.is_empty());
    }

    #[test]
    fn test_risk_bounds() {
        let settings = Settings::load().expect("Failed to load settings");
        assert!(settings.risk.max_notional_usd > 0.0);
        assert!(settings.risk.max_drawdown_daily > 0.0);
        assert!(settings.risk.signal_ttl_ms >= 5);
    }
}
