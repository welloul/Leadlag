# Handover Manual — Operations & Recovery

## Project Status

| Aspect | Status |
|--------|--------|
| **Phase** | Live Execution Readiness (v0.2.0) |
| **Stability** | All tests passing (v0.2.0 fixes verified) |
| **Live Trading** | Enabled (Hyperliquid Live Executor) |
| **API Keys** | HL_API_KEY / HL_API_SECRET required |
| **Active Strategy** | Impulse-OBI (Maker Mode + Alpha Decay Probes) |
| **Edge Window** | ~500ms Average Tokyo Decay |
| **Deployment** | AWS EC2 (13.231.81.63, ap-northeast-1) |

## Context for Next AI

### DO NOT CHANGE
1. **Ring buffer size** — Must remain power of 2 (currently 256)
2. **Crossbeam channels** — Do not replace with tokio::sync::mpsc
3. **Atomic kill switch** — Must use SeqCst ordering for safety
4. **ImpulseDetector venue routing** — Ticks must be routed to the correct tracker (tracker_a for Exchange A, tracker_b for Exchange B).
5. **MidpriceTracker warmup** — Both trackers must be `initialized` AND `warmed_up` before generating impulses.
6. **Per-venue simulator keying** — Matchers keyed by `(Symbol, VenueId)`. Each venue has independent order books.
7. **Local timestamps for freshness** — Use `SystemTime::now()` for freshness gating. Exchange timestamps are unreliable across venues (different clocks, drift).
8. **Side-aware cooldown** — Cooldown keyed by `(symbol, side)`, not just `symbol`. Allows valid reversals.
9. **$100 position cap (DYNAMIC)** — Calculated as `Filled + Pending`. Do not revert to static HashMaps; the current system is immune to state desync leaks.
10. **Reduce-Only on Exits** — All TP and Time-Exits MUST use `reduce_only: true` to prevent orphaned orders from reversing positions.

### KNOWN HACKS
1. `PreflightChecker::check_max_slippage` uses a size-impact model — approximate but sufficient.
2. **Main loop uses `tokio::task::yield_now()`** instead of `std::hint::spin_loop()`.
3. **Impulse sanity check** — Deltas > 500 bps are rejected as initialization artifacts.
4. **Conservative fill** — Only fills 50% of best level size. Real books shift during latency.
5. **Binance diff stream gap recovery** — When `prev_final_update_id != last_update_id`, the book is marked unsynced but doesn't re-fetch the REST snapshot. Needs a re-sync mechanism.
6. **Boot-Time Sync** — The executor MUST call `load_asset_context()` before trading starts. If the API returns a null meta state, trading must halt.

### RECENT FIXES (v0.2.0 — Passive Market Making & Alpha Decay)
1. **Passive Limit Lifecycle** — Bot now places `Post-Only` limit orders at mid-price instead of crossing the spread. Fees reduced to Maker tier (-1bp to 2bp rebate).
2. **Alpha Decay Probes** — High-resolution telemetry system measures local nanoseconds between lead breakout and laggard catch-up. Average Tokyo window: 500ms (BCH: 100ms, TON: 2.5s).
3. **Symbol-Specific Timeouts** — Replaced global `exit_timeout_ms` with a tiered lookup table. Fast coins (ADA, BCH) exit at 1.0s; slow coins (TON, ARB) hold for 2.5s.
4. **Configuration Hot-Reload** — Heartbeat loop re-reads `settings.toml` every 15s. Can adjust thresholds and timeouts live without dropping WebSocket connections.
5. **Take-Profit (TP) Expansion** — Global target increased to 13.0 bps for high-conviction maker entries.
6. **Limit-Matching Simulator** — Rebuilt `PaperSimulator` with async fill broadcasting and pending-order matching logic.
7. **OMS Pending Trackers** — Track positions by fill-time instead of submission-time to prevent exposure bloat during latency spikes.
8. **Dynamic Exposure Loop** — Ripped out brittle static maps (`cumulative_size`). OMS now recalculates exposure from `NetDelta` + `PendingOrders` on every signal.
9. **Boot-Time Meta Sync** — Executor fetches live asset indexes from Hyperliquid `/info` on startup. 
10. **Precise TP Sizing** — TP orders are now sized 1:1 against `fill.filled_size`, eliminating "Double TP" over-exposure.
11. **Reduce-Only Tunneling** — `OrderRequest` now maps the `reduce_only` flag directly to the Hyperliquid EIP-712 payload.

### RECENT FIXES (v0.1.5 — Signal Quality and Position Management Enhancements)
1. **Freshness gate 400ms** — Both venues must have received a tick within 400ms (local time). Prevents trading against stale data.
2. **Lag check restored** — `other_is_lagging` now checks `|delta| < 1.5 bps` instead of hardcoded `true`.
3. **Fees-aware spread check** — `entry_threshold_bps = 8` (covers taker fees ~5-10 bps + slippage). Direction-normalized edge calculation.
4. **Side-aware cooldown** — `(symbol, side)` key in OMS. 200ms cooldown between trades for same symbol+side. Allows reversals (BUY then SELL).
5. **Weighted OBI** — Depth-weighted: `weight = 1/(i+1)`. Top levels dominate.
6. **Time-based OBI persistence** — OBI must stay strong for 200ms (not count-based, which varies by venue tick rate).
7. **Conservative fill** — `allowed_size = best_level_size * 0.5`. Only fill half of what's visible.
8. **$100 position cap** — Direction-aware: if LONG, can SHORT to reduce but can't add more LONG.
9. **Per-symbol performance tracking** — Heartbeat shows fill rate per symbol.
10. **TTL 500ms** — Signal time-to-live reduced from 1500ms to 500ms.
11. **Combo window 150ms** — `signal_timeout_ms` increased from 10ms to 150ms.
12. **400ms book age gate** — Hard reject if target venue book > 400ms stale.

### RECENT FIXES (v0.1.4 — OBI-Impulse Bug Fixes and Retunes)
1. **Fixed freshness gate** — Now compares local wall-clock timestamps against `now_ns()`, preventing exchange timestamp drift from breaking staleness detection (was comparing exchange_ts vs wall_clock, causing underflow).
2. **Fixed combination logic** — HIGH conviction now requires directionally-consistent side only (`pending.side == incoming.side`), not venue matching. Eliminates impossible HIGH signals.
3. **Fixed signal expiry** — Pending signals expire via wall-clock `stored_at_ns` vs `now_ns()`, not unreliable exchange timestamps that break in simulation.
4. **Added momentum filter** — Impulse must agree in sign with previous delta to avoid reversion trades. Guards against false positives after price has already moved.
5. **Retuned parameters for signal quality** — `impulse_threshold_bps = 10` (was 5), `entry_threshold_bps = 5` (was 8), `obi_persist_ms = 30` (was 200), `signal_timeout_ms = 250` (was 150), `lag_threshold_bps = 1.0` (was 1.5).
6. **Schema precision** — `lag_threshold_bps` changed to `f64` for fractional bps thresholds.
7. **Per-symbol state isolation** — ImpulseObiEngine instances now properly isolated per symbol (handled in routing), preventing cross-symbol pollution.

### RECENT FIXES (v0.1.5 — Signal Quality and Position Management Enhancements)
1. **OBI wall-clock persistence** — Persistence timer uses `now_ns()` instead of exchange timestamps, immune to clock drift and stale simulation data.
2. **Venue-specific momentum filter** — B-sourced impulses check `tracker_b.last_delta_bps` (was incorrectly checking tracker_a), ensuring correct directional agreement.
3. **Per-symbol engine isolation** — 8 independent ImpulseObiEngine instances per symbol, eliminating cross-symbol signal pollution.
4. **Trending OBI condition** — Fires when both venues show positive imbalance but one is stronger (divergence in trending markets), expanding signal coverage beyond neutral-threshold logic.
5. **Impulse magnitude position sizing** — Position size scales with impulse strength (0.5–2× base), capped at 2× max_notional, for proportional risk-reward.
6. **Tighter exit and entry thresholds** — `exit_timeout_ms` reduced from 30s to 5s, `entry_threshold_bps` increased to 5.5 to ensure net-positive after fees and reduce dead position holding.

### RECENT FIXES (v0.1.2 — AWS Deployment & Real L2 Books)
1. Real L2 order book subscriptions (Binance `@depth@100ms`, Hyperliquid `l2Book`)
2. Local order book state with BTreeMap (Binance diff stream)
3. Symbol normalization (`ZECUSDT` → `ZEC`) for cross-venue keying
4. Per-venue spread model (Binance: 1 bps, Hyperliquid: 5 bps)
5. Staleness tracking per venue
6. Hyperliquid WebSocket `l2Book` channel parsing

### RECENT FIXES (v0.1.1 — Plan Review)
1. Impulse-OBI wired into main loop
2. PaperSimulator replaces MockExchange
3. ImpulseDetector cross-venue tracker pollution fixed
4. OMS error handling: `RiskError::ExecutionFailed(String)`
5. Signal clone eliminated via `PendingSignal`

## Recovery Procedures

### AWS Deployment

```bash
ssh -i ~/Desktop/tokio.pem ec2-user@13.231.81.63
pgrep tokioparasite                          # Check if running
tail -f ~/bot_debug.log                      # Live logs

# Restart
kill $(pgrep tokioparasite)
cd ~/tokioparasite
source ~/.cargo/env
cargo build --release
sed -i 's/use_real_data = false/use_real_data = true/' settings.toml
./target/release/tokioparasite > ~/bot_debug.log 2>&1 &
disown
```

## Configuration Reference

```toml
[strategy]
active_strategy = "impulse_obi"
symbols = ["ZEC", "WLD", "FARTCOIN", "DOGE", "SUI", "BCH", "PUMP", "ADA"]
impulse_threshold_bps = 10
lag_threshold_bps = 1.0
impulse_window_ms = 5
signal_timeout_ms = 250         # Combo window
min_trade_size_filter = 0.001
spread_filter_bps = 10

# Entry logic tightening (v0.1.5)
venue_freshness_ms = 400
entry_threshold_bps = 5.5       # Fees-aware minimum edge
cooldown_ms = 200               # Side-aware
max_levels_consumed = 3
obi_persist_ms = 30
fill_conservatism = 0.5

[risk]
max_notional_usd = 10.0         # Per-trade cap
max_drawdown_daily = 200.0
max_slippage_bps = 8
signal_ttl_ms = 500
exit_timeout_ms = 5000          # Exit dead positions after 5s
self_trade_prevention = true

# Position cap: $100 per (venue, symbol), direction-aware
# Implemented in OMS code, not in config
```

## Monitoring

| Message | Meaning |
|---------|---------|
| `HEARTBEAT` | Per-venue tick counters + per-symbol fill rates |
| `SYMBOLS:` | Per-symbol fill/reject counts and hit rates |
| `Impulse detected` | Price move detected (5-50 bps) |
| `Order submitted` | Fill confirmed |
| `Position cap` | Cumulative position hit $100 cap |
| `Cooldown` | Side-aware cooldown blocked trade |
| `POSITIONS` | Periodic PnL snapshot with per-symbol breakdown |
| `No book` | Target venue has no L2 book data |
| `Book gap` | Binance diff stream continuity broken |

### AWS Status

```bash
ssh -i ~/Desktop/tokio.pem ec2-user@13.231.81.63 "grep HEARTBEAT ~/bot_debug.log | tail -3"
ssh -i ~/Desktop/tokio.pem ec2-user@13.231.81.63 "grep -A10 POSITIONS ~/bot_debug.log | tail -12"
ssh -i ~/Desktop/tokio.pem ec2-user@13.231.81.63 "grep 'Position cap' ~/bot_debug.log | wc -l"
```
