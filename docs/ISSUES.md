# Engineering Issues Log

## Hot Path

| # | Issue | Status |
|---|-------|--------|
| H1 | Ring buffer not using bitwise mask | ✅ Resolved |
| H2 | Correlation uses `sqrt()` (slow on ARM) | ⚠️ Deferred |
| H3 | No SIMD vectorization for lag search | ⚠️ Deferred |
| H4 | `f64::is_finite()` check adds branch | ✅ Resolved |
| H5 | Time-grid alignment allocates Vec per tick | ✅ Resolved |
| H6 | ImpulseDetector cross-venue tracker pollution | ✅ Resolved |
| H7 | NaN/Inf in MidpriceTracker | ✅ Resolved |
| H8 | Signal clone on hot path | ✅ Resolved |
| H9 | Impulse spike artifacts (382k bps) | ✅ Resolved — warmup + sanity |
| H10 | `other_is_lagging = true` hardcoded | ✅ Resolved — restored delta check |
| H11 | Exchange timestamps unreliable across venues | ✅ Resolved — local timestamps for freshness |

## OMS

| # | Issue | Status |
|---|-------|--------|
| O1 | HashMap lookup for net_delta | ✅ Resolved |
| O2 | Preflight checks run sequentially | ⚠️ Deferred |
| O3 | Self-trade prevention O(n) | ✅ Resolved |
| O4 | Kill switch SeqCst ordering | ⚠️ Deferred |
| O5 | Execution errors swallowed | ✅ Resolved |
| O6 | Correlation check blocks Impulse-OBI | ✅ Resolved |
| O7 | No position-level notional cap | ✅ Resolved — $100 cap, direction-aware |
| O8 | No cooldown between trades | ✅ Resolved — side-aware 200ms |
| O9 | No book consumption model | ✅ Resolved — 50% of best level |
| O10 | Taker fee drag (entry fee > edge) | ✅ Resolved — Shift to Post-Only Maker |
| O11 | Manual exit management (laggard) | ✅ Resolved — Auto-TP + Tiered symbol timeouts |

## Simulator

| # | Issue | Status |
|---|-------|--------|
| S1 | Matchers keyed by Symbol only | ✅ Resolved |
| S2 | `entry().or_insert_with()` in simulate_fill | ✅ Resolved |
| S3 | Execution price from source tick | ✅ Resolved |
| S4 | Single global spread model | ✅ Resolved |
| S5 | No other-venue book seeding | ✅ Resolved — removed fake seeding |
| S6 | No conservative fill model | ✅ Resolved — 50% of best level |
| S7 | No best_bid_size/best_ask_size | ✅ Resolved |

## Signal Processing

| # | Issue | Status |
|---|-------|--------|
| SP1 | OBI simple (equal weight per level) | ✅ Resolved — depth-weighted |
| SP2 | OBI flicker/spoofing vulnerable | ✅ Resolved — time-based persistence |
| SP3 | No cross-venue edge check | ✅ Resolved — fees-aware entry threshold |
| SP4 | Combo window too tight (10ms) | ✅ Resolved — 150ms |
| SP5 | Blind exits/timeouts | ✅ Resolved — Alpha Decay Probes telemetry |

## Infra

| # | Issue | Status |
|---|-------|--------|
| I1 | No CPU pinning on macOS | ✅ Resolved |
| I2 | Telemetry writer uses BufWriter | ⚠️ Deferred |
| I3 | Sled DB flush blocks on drop | ✅ Resolved |
| I4 | No graceful WebSocket reconnection | ⚠️ Deferred |
| I5 | Starlink latency spikes not detected | ⚠️ Deferred |
| I6 | Book subscriptions not available for live exchanges | ✅ Resolved — real L2 streams |
| I7 | Main loop uses `yield_now()` | ⚠️ Deferred |
| I8 | Hyperliquid WS parsing broken | ✅ Resolved |
| I9 | Binance diff stream gap re-sync | ⚠️ Deferred |
| I10 | No per-symbol performance tracking | ✅ Resolved — heartbeat |
| I11 | Static configuration (restart required) | ✅ Resolved — 15s Hot-Reload heartbeat |

## Known Technical Debt

1. ~65 compiler warnings for unused code.
2. No live exchange `OrderExecution`.
3. No Prometheus metrics.
4. Binance diff stream gap detection doesn't re-fetch snapshot.
5. Low fill rate with Hyperliquid (~1 tick/sec vs Binance ~79/sec). Lag check filters most HL signals.

## Priority Matrix

| Priority | Issue | Impact | Effort |
|----------|-------|--------|--------|
| P0 | Binance diff stream re-sync | Book accuracy | Medium |
| P1 | WebSocket reconnection | Production resilience | Medium |
| P1 | Live exchange book subscriptions | OBI live capability | High |
| P2 | Prometheus metrics | Monitoring | Medium |
| P2 | SIMD lag search | Latency | High |
| P3 | Compiler warnings cleanup | Code quality | Low |

## AWS Performance (v0.1.3)

| Metric | Value |
|--------|-------|
| Binance tick rate | ~79/sec |
| Hyperliquid tick rate | ~1.3/sec |
| Binance L2 book rate | ~80 updates/sec |
| Hyperliquid L2 book rate | ~4 updates/sec |
| Fresh book ratio | 100% |
| Stale book entries | 0 |
| Max notional per symbol | $100 (cap enforced) |
