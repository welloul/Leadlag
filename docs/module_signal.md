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
  - Momentum filter: Current delta must agree with previous delta sign
  - Sanity check: Rejects deltas > 500 bps (initialization artifacts)

### ObiDivergenceDetector (`obi_divergence.rs`)
- **process_book(book: &BookUpdate) -> Option<ObiSignal>**
  - Input: Order book snapshot with bids/asks, venue, timestamp
  - Output: ObiSignal if weighted OBI persists > 30ms and divergence detected
  - Side effects: Maintains persistence timers per venue
- **Key components:**
  - Weighted OBI: Depth-weighted imbalance (1/(i+1) for levels i=0..)
  - Time-based persistence: Signal must hold for 30ms minimum
  - Divergence logic: One venue strong imbalance, other neutral

### ImpulseObiEngine (`impulse_obi.rs`)
- **process_signal(signal: Signal) -> Option<Conviction>**
  - Input: Incoming ImpulseSignal or ObiSignal
  - Output: HIGH/MEDIUM conviction if combination or solo thresholds met
  - Side effects: Manages pending signals with 250ms timeout, clears expired
- **Key components:**
  - Combination logic: HIGH if pending + incoming have matching sides
  - Solo logic: MEDIUM for impulse-only or OBI-only signals
  - Timeout: Wall-clock based expiry to prevent simulation drift

### Module Router (`mod.rs`)
- **process_tick(tick: Arc<Tick>) -> Vec<Signal>**
  - Routes ticks to per-symbol ImpulseDetector instances
- **process_book(book: Arc<BookUpdate>) -> Vec<Signal>**
  - Routes books to per-symbol ObiDivergenceDetector instances
- **combine_signals(signals: Vec<Signal>) -> Vec<Conviction>**
  - Passes signals through ImpulseObiEngine per symbol

## The "Hurdles"

### Fixed Bugs (v0.1.4)
- **Freshness gate drift**: Exchange timestamps unreliable; fixed by using wall-clock comparisons (was causing 100% stale rejections in simulation).
- **Impossible HIGH convictions**: Venue matching requirement made combinations impossible; fixed to side-only matching.
- **Simulation expiry breakage**: Exchange timestamp expiry broke in stale clocks; fixed with wall-clock stored_at_ns.
- **Cross-symbol pollution**: Single engine instance mixed symbols; fixed with per-symbol routing.

### Remaining Technical Debt
- **Race conditions**: Per-symbol state isolation may have concurrent access issues under high load (needs atomic guards).
- **Parameter sensitivity**: Retuned parameters (impulse_threshold=10bps, entry_threshold=5bps) unvalidated in live conditions.
- **Book reconstruction gaps**: Binance diff streams may have continuity breaks; lacks re-sync mechanism.
- **Memory pressure**: BTreeMap for order books grows unbounded; needs size limits.

## Future Roadmap

- **Signal diversification**: Add VWAP divergence, volume impulse, and spread impulse detectors.
- **Adaptive parameters**: Implement ML-based threshold adjustment based on volatility and correlation.
- **Backtesting framework**: Integrate historical data replay for parameter optimization.
- **Signal quality metrics**: Add conviction scores, false positive tracking, and performance attribution per signal type.
- **Latency profiling**: Instrument hot path with nanosecond timers for bottleneck identification.
- **Cross-venue reconciliation**: Handle symbol normalization edge cases (e.g., BTC vs BTCUSDT).
- **Refactor for modularity**: Split detectors into traits for easier testing and extension.