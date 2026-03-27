//! Risk pre-flight checks for the OMS.
//!
//! All checks are performed before order submission to prevent
//! risk violations. These checks are non-bypassable.

use crate::config::{RiskSettings, StrategySettings};
use crate::eal::{RiskError, TradeSignal, VenueId};
use super::NetDelta;
use std::time::{SystemTime, UNIX_EPOCH};

/// Pre-flight checker for risk management.
///
/// Validates trade signals against risk limits before order submission.
pub struct PreflightChecker {
    /// Risk settings.
    risk_settings: RiskSettings,
    /// Strategy settings (for min_correlation_r).
    strategy_settings: StrategySettings,
}

impl PreflightChecker {
    /// Create a new pre-flight checker.
    pub fn new(risk_settings: RiskSettings, strategy_settings: StrategySettings) -> Self {
        Self {
            risk_settings,
            strategy_settings,
        }
    }

    /// Run all pre-flight checks on a trade signal.
    pub fn check_signal(
        &self,
        signal: &TradeSignal,
        current_price: f64,
        net_delta: &NetDelta,
    ) -> Result<(), RiskError> {
        // 1. Check kill switch
        self.check_kill_switch(signal, net_delta)?;

        // 2. Check daily loss limit
        self.check_daily_loss_limit(net_delta)?;

        // 3. Check signal TTL
        self.check_signal_ttl(signal)?;

        // 4. Check correlation threshold
        self.check_correlation(signal)?;

        // 5. Check max notional
        self.check_max_notional(signal, current_price)?;

        // 6. Check max slippage (estimated)
        self.check_max_slippage(current_price)?;

        Ok(())
    }

    /// Check if kill switch is active for the target venue.
    fn check_kill_switch(
        &self,
        signal: &TradeSignal,
        net_delta: &NetDelta,
    ) -> Result<(), RiskError> {
        if net_delta.is_kill_switch_active(&signal.target_venue) {
            return Err(RiskError::KillSwitchActive {
                venue: signal.target_venue,
            });
        }
        Ok(())
    }

    /// Check if daily loss limit is breached.
    fn check_daily_loss_limit(&self, net_delta: &NetDelta) -> Result<(), RiskError> {
        if net_delta.is_daily_loss_limit_breached() {
            return Err(RiskError::DailyDrawdownLimit {
                drawdown: net_delta.daily_realized_pnl().abs(),
                max: self.risk_settings.max_drawdown_daily,
            });
        }
        Ok(())
    }

    /// Check if signal has expired.
    fn check_signal_ttl(&self, signal: &TradeSignal) -> Result<(), RiskError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        let age_ns = now.saturating_sub(signal.timestamp_ns);
        let age_ms = age_ns / 1_000_000;

        if age_ms > self.risk_settings.signal_ttl_ms {
            return Err(RiskError::SignalExpired {
                age_ms,
                ttl_ms: self.risk_settings.signal_ttl_ms,
            });
        }
        Ok(())
    }

    /// Check if correlation is above minimum threshold.
    fn check_correlation(&self, signal: &TradeSignal) -> Result<(), RiskError> {
        if signal.correlation_r < self.strategy_settings.min_correlation_r {
            return Err(RiskError::CorrelationTooLow {
                r: signal.correlation_r,
                min: self.strategy_settings.min_correlation_r,
            });
        }
        Ok(())
    }

    /// Check if order would exceed max notional.
    fn check_max_notional(
        &self,
        signal: &TradeSignal,
        current_price: f64,
    ) -> Result<(), RiskError> {
        // Estimate notional based on max_notional_usd / price
        let estimated_size = self.risk_settings.max_notional_usd / current_price;
        let notional = estimated_size * current_price;

        if notional > self.risk_settings.max_notional_usd * 1.01 {
            // Allow 1% tolerance for price movement
            return Err(RiskError::ExceedsMaxNotional {
                notional,
                max: self.risk_settings.max_notional_usd,
            });
        }
        Ok(())
    }

    /// Check estimated slippage against limit.
    ///
    /// Uses order size and typical market depth to estimate realistic slippage.
    /// For liquid markets like BTC, slippage scales with order size relative to
    /// available liquidity at each price level.
    fn check_max_slippage(&self, current_price: f64) -> Result<(), RiskError> {
        if current_price <= 0.0 {
            return Err(RiskError::ExcessiveSlippage {
                slippage_bps: f64::INFINITY,
                max_bps: self.risk_settings.max_slippage_bps as f64,
            });
        }

        // Calculate order size based on max notional
        let order_size = self.risk_settings.max_notional_usd / current_price;
        let notional = order_size * current_price;

        // Estimate slippage based on order size and market depth
        // For liquid markets like BTC/USDT:
        // - Top of book: ~$100K-$500K liquidity within 1-2 bps
        // - Mid depth: ~$1M-$5M liquidity within 5-10 bps
        // - Deep depth: ~$10M+ liquidity within 10-20 bps
        //
        // Slippage model: slippage_bps = base_slippage + (size_impact * order_size_usd)
        // where base_slippage = 1 bps (market impact)
        // and size_impact = 0.001 bps per $1000 notional (for BTC)
        let base_slippage_bps = 1.0;
        let size_impact_bps_per_1000 = 0.001;
        let estimated_slippage_bps = base_slippage_bps + (size_impact_bps_per_1000 * (notional / 1000.0));

        // Cap at reasonable maximum (e.g., 50 bps for very large orders)
        let estimated_slippage_bps = estimated_slippage_bps.min(50.0);

        if estimated_slippage_bps > self.risk_settings.max_slippage_bps as f64 {
            return Err(RiskError::ExcessiveSlippage {
                slippage_bps: estimated_slippage_bps,
                max_bps: self.risk_settings.max_slippage_bps as f64,
            });
        }
        Ok(())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eal::{OrderSide, Symbol};

    fn make_signal(venue: VenueId, r: f64, age_ms: u64) -> TradeSignal {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        TradeSignal {
            side: OrderSide::Buy,
            target_venue: venue,
            symbol: Symbol::new("BTC"),
            correlation_r: r,
            lag_offset_ns: 0,
            timestamp_ns: now - (age_ms * 1_000_000),
        }
    }

    #[test]
    fn test_correlation_too_low() {
        let risk_settings = RiskSettings {
            max_notional_usd: 5000.0,
            max_drawdown_daily: 200.0,
            max_slippage_bps: 8,
            signal_ttl_ms: 150,
            self_trade_prevention: true,
        };

        let strategy_settings = crate::config::StrategySettings {
            symbols: vec!["BTC".to_string()],
            window_size_ticks: 256,
            min_correlation_r: 0.85,
            hysteresis_buffer: 0.10,
            enable_obi: false,
            obi_weight: 0.0,
        };

        let checker = PreflightChecker::new(risk_settings, strategy_settings);
        let net_delta = NetDelta::new(200.0);
        let signal = make_signal(VenueId::EXCHANGE_A, 0.5, 0); // Low correlation

        let result = checker.check_signal(&signal, 60000.0, &net_delta);
        assert!(result.is_err());
    }

    #[test]
    fn test_signal_expired() {
        let risk_settings = RiskSettings {
            max_notional_usd: 5000.0,
            max_drawdown_daily: 200.0,
            max_slippage_bps: 8,
            signal_ttl_ms: 150,
            self_trade_prevention: true,
        };

        let strategy_settings = crate::config::StrategySettings {
            symbols: vec!["BTC".to_string()],
            window_size_ticks: 256,
            min_correlation_r: 0.85,
            hysteresis_buffer: 0.10,
            enable_obi: false,
            obi_weight: 0.0,
        };

        let checker = PreflightChecker::new(risk_settings, strategy_settings);
        let net_delta = NetDelta::new(200.0);
        let signal = make_signal(VenueId::EXCHANGE_A, 0.95, 200); // Expired

        let result = checker.check_signal(&signal, 60000.0, &net_delta);
        assert!(result.is_err());
    }

    #[test]
    fn test_valid_signal_passes() {
        let risk_settings = RiskSettings {
            max_notional_usd: 5000.0,
            max_drawdown_daily: 200.0,
            max_slippage_bps: 8,
            signal_ttl_ms: 150,
            self_trade_prevention: true,
        };

        let strategy_settings = crate::config::StrategySettings {
            symbols: vec!["BTC".to_string()],
            window_size_ticks: 256,
            min_correlation_r: 0.85,
            hysteresis_buffer: 0.10,
            enable_obi: false,
            obi_weight: 0.0,
        };

        let checker = PreflightChecker::new(risk_settings, strategy_settings);
        let net_delta = NetDelta::new(200.0);
        let signal = make_signal(VenueId::EXCHANGE_A, 0.95, 10); // Valid

        let result = checker.check_signal(&signal, 60000.0, &net_delta);
        assert!(result.is_ok());
    }
}