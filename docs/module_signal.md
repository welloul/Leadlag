# Module: Signal Processing (`src/signal/`)

## Responsibility

The signal module is responsible for detecting microsecond-level price impulses and order book imbalances across cryptocurrency exchanges. It generates trading signals for the lead-lag arbitrage strategy by processing real-time tick data and order book updates. The module ensures signals are fresh, directionally consistent, and meet edge thresholds before routing to the Order Management System (OMS). Its primary goal is to identify exploitable cross-venue divergences with high precision and low latency, while maintaining strict risk controls through freshness gates and momentum filters.

## Key Logic & Functions

### ImpulseDetector (`impulse.rs`)
- **process_tick(tick: &Tick) -> Option<ImpulseSignal>**
  - Input: Market tick with price, venue, timestamp
  - Output: ImpulseSignal if delta exceeds threshold and all gates pass
  - Side effects: Updates internal MidpriceTracker state per venue
- **Key components:**
  - MidpriceTracker: Maintains running midprice per venue with warmup logic
  - Freshness gate: Compares local wall-clock timestamps (< 400ms stale)
  - Lag check: Ensures other venue delta < 1.0 bps
  - Momentum filter: Current delta must agree with venue-specific previous delta sign (tracker_a.last_delta_bps for A-source, tracker_b for B-source)
  - Sanity check: Rejects deltas > 500 bps (initialization artifacts)

### ObiDivergenceDetector (`obi_divergence.rs`)
- **process_book(book: &BookUpdate) -> Option<ObiSignal>**
  - Input: Order book snapshot with bids/asks, venue, timestamp
  - Output: ObiSignal if weighted OBI persists > 30ms wall-clock and divergence detected
  - Side effects: Maintains persistence timers per venue using `now_ns()`
- **Key components:**
  - Weighted OBI: Depth-weighted imbalance (1/(i+1) for levels i=0..)
  - Time-based persistence: Signal must hold for 30ms minimum (wall-clock immune to drift)
  - Divergence logic: Both venues positive imbalance, but one stronger (trending market condition)

### ImpulseObiEngine (`impulse_obi.rs`)
- **process_signal(signal: Signal) -> Option<Conviction>**
  - Input: Incoming ImpulseSignal or ObiSignal
  - Output: HIGH/MEDIUM conviction if combination or solo thresholds met
- **Conviction Logic (v0.2.0):**
  - **HIGH**: Pending signal exists with the same side.
  - **MEDIUM**: Solo impulse or solo OBI.
  - **Alpha Decay Probes**: Instruments the leader move vs. laggard convergence window to derive optimal timeouts.

### Module Router (`mod.rs`)
- **update_settings(settings: StrategySettings)**: Propagates live `settings.toml` updates during 15-second heart-beat hot-reloads. Reinitializes thresholds and per-symbol conviction gates.

## Fixed Bugs (v0.2.0)
- **Starknet/ZEC Drift**: Fixed symbol normalization that caused cross-pair signal leaks; unified symbol keys in metadata.
- **Asymmetric Latency**: Hyperliquid l2Book lag addressed with wall-clock OBI persistence (30ms).
- **Conviction Match Failure**: Relaxed combination logic to side-only matching (previously required venue matching, which were disjoint).
- **Stale Settings**: Replaced static configuration with dynamic reloader to avoid bot restarts during parameter tuning.

### Remaining Technical Debt
- **Race conditions**: Per-symbol state isolation may have concurrent access issues under high load (needs atomic guards).
- **Parameter sensitivity**: Retuned parameters (entry_threshold=5.5bps) unvalidated in live conditions.
- **Book reconstruction gaps**: Binance diff streams may have continuity breaks; lacks re-sync mechanism.
- **Memory pressure**: BTreeMap for order books grows unbounded; needs size limits.
- **Sizing validation**: Impulse-based sizing needs backtesting for optimal scaling curve.

## Future Roadmap

- **Signal diversification**: Add VWAP divergence, volume impulse, and spread impulse detectors.
- **Adaptive parameters**: Implement ML-based threshold adjustment based on volatility and correlation.
- **Backtesting framework**: Integrate historical data replay for parameter optimization and sizing validation.
- **Signal quality metrics**: Add conviction scores, false positive tracking, and performance attribution per signal type.
- **Latency profiling**: Instrument hot path with nanosecond timers for bottleneck identification.
- **Cross-venue reconciliation**: Handle symbol normalization edge cases (e.g., BTC vs BTCUSDT).
- **Refactor for modularity**: Split detectors into traits for easier testing and extension.
- **Position sizing optimization**: Research Kelly criterion or similar for impulse-magnitude scaling.