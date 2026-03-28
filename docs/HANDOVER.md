# Handover Manual — Operations & Recovery

## Project Status

| Aspect | Status |
|--------|--------|
| **Phase** | Paper Trading (v0.1.1) |
| **Stability** | All 137 tests passing (132 unit + 7 integration + 4 signal flow + 1 doc test) |
| **Live Trading** | Disabled (simulation only) |
| **API Keys** | Hyperliquid only (Binance pending) |
| **Active Strategy** | Impulse-OBI or Correlation-Hysteresis (configurable via settings.toml) |

## Context for Next AI

### DO NOT CHANGE
1. **Ring buffer size** — Must remain power of 2 (currently 256)
2. **Hot path thread model** — Dedicated OS thread with spin-loop
3. **Crossbeam channels** — Do not replace with tokio::sync::mpsc
4. **Atomic kill switch** — Must use SeqCst ordering for safety
5. **ImpulseDetector venue routing** — Ticks must be routed to the correct tracker (tracker_a for Exchange A, tracker_b for Exchange B). Cross-pollution of trackers was a critical bug fixed in v0.1.1.

### KNOWN HACKS (Updated v0.1.1)
1. `TimeGrid::ingest_tick` returns `IngestResult` with fixed-size array — no longer allocates. ✅ Resolved.
2. `NetDelta` uses fixed-size array `[[Option<Position>; MAX_SYMBOLS]; MAX_VENUES]` — O(1) lookup. ✅ Resolved.
3. `PreflightChecker::check_max_slippage` uses a size-impact model based on order notional — approximate but better than the old hardcoded 1 bps.
4. **Book subscriptions not available for live exchanges** — `BinanceExchange::subscribe_book()` and `HyperliquidExchange::subscribe_book()` return `Err(Not implemented)`. The OBI strategy works in mock mode only. When adding live book data, implement these methods.
5. **Main loop uses `tokio::task::yield_now()`** instead of `std::hint::spin_loop()` — the async runtime requires cooperative scheduling. For true spin-loop performance, the hot path would need to run on a dedicated OS thread (see architecture.md).

### RECENT FIXES (v0.1.1 — Plan Review)
1. **Impulse-OBI wired into main loop** — `pipeline.process_tick()` and `pipeline.process_book()` now called from main.rs. Previously, the Impulse-OBI strategy returned `None` from `process_pair()` and had no alternative entry point, resulting in zero signals.
2. **Book subscriptions added** — main.rs subscribes to order book data when `active_strategy = "impulse_obi"`. Falls back gracefully when live exchanges don't implement `subscribe_book()`.
3. **PaperSimulator used instead of MockExchange** — main.rs now uses `PaperSimulator` for order execution, providing realistic L2 matching, slippage, and fees. Previously used `MockExchange` which had zero slippage/fees.
4. **Duplicate TimeGrid removed** — `SignalPipeline` no longer has an internal `TimeGrid`. It uses a `timegrid_precision_ns` field set from main.rs settings.
5. **ImpulseDetector cross-venue bug fixed** — Ticks were being routed to BOTH trackers, corrupting delta calculations. Now correctly routes to tracker_a for Exchange A and tracker_b for Exchange B only.
6. **NaN/Inf guards in MidpriceTracker** — Added `is_finite()` and `> 0.0` checks to prevent division by zero and NaN propagation.
7. **OMS error handling fixed** — `RiskError::ExecutionFailed(String)` variant added. Previously, execution errors were discarded and replaced with a misleading `ExceedsMaxNotional` error with zeroed values.
8. **Integration tests fixed** — All `StrategySettings` constructors updated with 12 missing Impulse-OBI fields. Tests were non-compiling.
9. **Hysteresis test corrected** — `test_no_flip_below_threshold_with_higher_values` renamed to `test_no_flip_when_current_lead_reasserts` with accurate comments describing streak-based behavior.
10. **Signal clone eliminated** — `ImpulseObiEngine` now stores `PendingSignal` (Copy-friendly) instead of cloning full `ImpulseSignal`/`ObiSignal` structs with heap-allocated `Symbol` strings.

### RECENT FIXES (v0.1.0)
1. **Catastrophic Cancellation** — Pearson correlation formula lost precision with large prices (~60,000). Fixed with numerically stable mean-subtraction formula.
2. **Negative Lag Indexing** — `(i as i32 + lag) as usize` wrapped to `usize::MAX` when negative. Fixed with explicit bounds check.
3. **Hysteresis Magnitude Check** — Required `new_r > current_r + threshold_margin` which failed with high-correlation data. Fixed to flip based on consistent leader change (streak).
4. **Directional Lag Comparison** — Was using hardcoded ±10 instead of actual `best_lag`. Fixed to use `find_best_lag()` result.
5. **Half-Window Warmup** — Required `window_size_ticks / 2` samples, but lag detection needs full window. Changed to `window_size_ticks`.

## Recovery Procedures

### Starlink Outage

```bash
# 1. Check connectivity
ping -c 3 api.hyperliquid.xyz

# 2. If down, switch to fiber failover
sudo ip route change default via <fiber_gateway>

# 3. Restart bot with failover config
RUST_LOG=info cargo run --release
```

### Solar Power Cycling

```bash
# 1. Check UPS status
upsc ups

# 2. If battery low, gracefully shutdown
kill -SIGTERM $(pgrep tokioparasite)

# 3. Wait for clean shutdown (check logs)
tail -f /var/log/tokioparasite.log

# 4. Power restored — restart
cargo run --release
```

### Manual Kill-Switch Reset

```bash
# 1. Check current kill switch state
# (via CLI or logs)

# 2. Reset via Sled DB
# The kill switch is stored in memory only (AtomicBool)
# Restarting the bot resets it automatically

# 3. If stuck, force restart
kill -9 $(pgrep tokioparasite)
cargo run --release
```

### State Recovery After Crash

```bash
# 1. Check Sled DB for last known state
ls -la data/state_db/

# 2. Bot automatically loads:
# - Last known positions
# - Daily realized PnL
# - Nonce counters

# 3. Verify recovery
RUST_LOG=tokioparasite=info cargo run --release
# Look for "State store opened" in logs
```

## Configuration Reference

### Strategy Toggle

The bot supports two strategies, selectable via `active_strategy` in settings.toml:

| Strategy | Description | Best For |
|----------|-------------|----------|
| `correlation_hysteresis` | Statistical lead-lag detection | Slow, confirmed signals |
| `impulse_obi` | Event-driven microstructure alpha | Fast, immediate signals |

### Impulse-OBI Strategy

**Datapoint 1: Trade Impulse**
- Detects fast price moves on one exchange while other lags
- Threshold: 5 bps impulse, 1.5 bps lag
- Window: 5ms lookback
- Action: Trade the laggard

**Datapoint 2: OBI Divergence**
- Detects order book imbalance divergence
- Strong threshold: 0.7 (bid-heavy or ask-heavy)
- Neutral threshold: 0.2
- Action: Trade the neutral exchange

**Combined Signal Priority:**
- Impulse + OBI confirms → HIGH conviction
- Impulse only → MEDIUM conviction
- OBI only → MEDIUM conviction

### settings.toml Key Fields

```toml
[strategy]
active_strategy = "impulse_obi"  # or "correlation_hysteresis"
symbols = ["ZEC", "XMR", "LINK"]

# Impulse-OBI settings
impulse_threshold_bps = 5
lag_threshold_bps = 1.5
impulse_window_ms = 5
signal_timeout_ms = 10
min_trade_size_filter = 0.001
spread_filter_bps = 10

# OBI settings
obi_strong_threshold = 0.7
obi_neutral_threshold = 0.2
obi_depth = 5
obi_spike_threshold = 0.3

[app]
cpu_pinning = true      # Set false for macOS development
perf_mode = true        # Enables spin-loop (100% CPU)

[simulation]
enabled = true          # Paper trading (MUST be true until validated)
use_real_data = true    # Fetch real market data (no API keys needed)

[risk]
max_notional_usd = 5000.0   # Max position size
max_drawdown_daily = 200.0  # Daily loss limit
signal_ttl_ms = 150         # Signal expiration
```

### Environment Variables

```bash
# API Keys (never commit to git)
export BINANCE_API_KEY="your_key"
export BINANCE_API_SECRET="your_secret"
export HL_API_KEY="your_key"
export HL_API_SECRET="your_secret"

# Logging
export RUST_LOG=tokioparasite=info
```

## Monitoring

### Key Log Messages

| Message | Meaning | Action |
|---------|---------|--------|
| `Kill switch activated!` | Risk limit breached | Check daily PnL |
| `Processed N ticks` | Normal operation | None |
| `Signal generated` | Trade opportunity (correlation-hysteresis) | Check OMS logs |
| `Impulse signal` | Trade opportunity (impulse-obi) | Check OMS logs |
| `OBI signal` | Trade opportunity (obi divergence) | Check OMS logs |
| `Connection lost` | Exchange down | Check network |
| `Order rejected` | Risk check or execution failure | Check error message |

### Telemetry Files

```
data/telemetry/
├── telemetry_1711468800.bin  # Binary tick data
├── telemetry_1711472400.bin  # Rotated hourly
└── ...
```

**Format:** Custom binary (not Proto3 yet)
- Byte 0: Entry type (0x01=Tick, 0x02=LeadLag, 0x03=Signal)
- Bytes 1+: Type-specific payload

## Emergency Contacts

| Role | Contact | When |
|------|---------|------|
| Developer | [Your contact] | Code issues |
| Exchange Support | Hyperliquid Discord | API problems |
| Network | Starlink Support | Connectivity |
