# Engineering Issues Log

## Hot Path

| # | Issue | Latency Impact | Resolution Status |
|---|-------|----------------|-------------------|
| H1 | Ring buffer not using bitwise mask | +50ns per push | ✅ Resolved |
| H2 | Correlation uses `sqrt()` (slow on ARM) | +200ns per calc | ⚠️ Deferred |
| H3 | No SIMD vectorization for lag search | +500ns for 21 lags | ⚠️ Deferred |
| H4 | `f64::is_finite()` check adds branch | +10ns per calc | ✅ Resolved |
| H5 | Time-grid alignment allocates Vec per tick | +1µs per tick | ✅ Resolved |
| H6 | ImpulseDetector cross-venue tracker pollution | Corrupted deltas | ✅ Resolved |
| H7 | NaN/Inf in MidpriceTracker | Invalid signals | ✅ Resolved |
| H8 | Signal clone on hot path | +1 allocation/signal | ✅ Resolved |
| H9 | Impulse spike artifacts (382k bps) | False signals | ✅ Resolved — warmup + sanity |
| H10 | `other_is_lagging = true` when `None` | False signals | ✅ Resolved — `None` → `false` |

## OMS

| # | Issue | Latency Impact | Resolution Status |
|---|-------|----------------|-------------------|
| O1 | HashMap lookup for net_delta | +200ns per lookup | ✅ Resolved |
| O2 | Preflight checks run sequentially | +500ns total | ⚠️ Deferred |
| O3 | Self-trade prevention O(n) | O(n) per check | ✅ Resolved |
| O4 | Kill switch SeqCst ordering | +50ns per check | ⚠️ Deferred |
| O5 | Execution errors swallowed | Debugging impossible | ✅ Resolved |
| O6 | Correlation check blocks Impulse-OBI | Wrong strategy gating | ✅ Resolved — skipped for impulse_obi |

## Simulator

| # | Issue | Impact | Resolution Status |
|---|-------|--------|-------------------|
| S1 | Matchers keyed by Symbol only | Wrong-venue fills | ✅ Resolved — `(Symbol, VenueId)` |
| S2 | `entry().or_insert_with()` in simulate_fill | Silent empty book creation | ✅ Resolved — `get_mut()` |
| S3 | Execution price from source tick | Wrong price for cross-venue | ✅ Resolved — `get_mid_price()` |
| S4 | Single global spread model | Unrealistic fills | ✅ Resolved — per-venue spread |
| S5 | No other-venue book seeding | One venue always empty | ✅ Resolved — seed from first tick |

## Infra

| # | Issue | Resolution Status |
|---|-------|-------------------|
| I1 | No CPU pinning on macOS | ✅ Resolved |
| I2 | Telemetry writer uses BufWriter | ⚠️ Deferred |
| I3 | Sled DB flush blocks on drop | ✅ Resolved |
| I4 | No graceful WebSocket reconnection | ⚠️ Deferred |
| I5 | Starlink latency spikes not detected | ⚠️ Deferred |
| I6 | Book subscriptions not available for live exchanges | ⚠️ Deferred — synthetic books used |
| I7 | Main loop uses `yield_now()` instead of spin-loop | ⚠️ Deferred |
| I8 | Hyperliquid WS parsing broken | ✅ Resolved — channel-wrapped format |

## Known Technical Debt

1. ~30 compiler warnings for unused code.
2. No live exchange `OrderExecution` — only `MarketData` for Binance/Hyperliquid.
3. No Prometheus metrics.
4. Low fill rate with Hyperliquid (~1 tick/sec vs Binance ~18/sec). The `other_is_lagging` check requires both venues to have recent deltas.
5. "No book" errors when target venue book isn't populated before signal executes.
6. ZEC/LINK initial price delta is massive between venues. Warmup handles this but limits signal generation for these symbols.

---

## AWS Deployment Findings (v0.1.2)

### Symbol Performance (20-min run: ZEC WLD FARTCOIN DOGE SUI BCH PUMP ADA)

| Symbol | Impulses | Binance ticks | HL ticks | Notes |
|--------|----------|---------------|----------|-------|
| PUMP | 52 | High | Medium | Most volatile, frequent signals |
| WLD | 12 | High | Medium | Active on both venues |
| ZEC | 10 | Medium | Medium | Massive initial delta handled by sanity |
| FARTCOIN | 3 | High | Low | HL ticks too slow for lagging check |
| BCH | 1 | High | Low | Same issue |
| BTC, ETH, SOL, TAO | Mixed | Very high | Low | HL rate too slow |
| DOGE, SUI, ADA | 0 | High | Low | No impulses detected |

### Root Cause of Low Fill Rate

The `other_is_lagging` check requires:
1. Both trackers `initialized` (have at least 1 tick)
2. Both trackers `warmed_up` (completed 1 full window cycle)
3. The other tracker's `current_delta()` returns `Some(d)` with `|d| < 1.5 bps`

With Hyperliquid sending ~1 tick/sec and Binance sending ~18/sec, the other tracker's delta is often `None` (window hasn't elapsed on HL's side) → `other_is_lagging = false` → no order.

**Potential fixes:**
- Increase `impulse_window_ms` from 5 to 50-100ms to match HL's tick rate
- Use `None` = lagging (was original behavior, removed due to false spikes)
- Require only `initialized` check, not `warmed_up` (but this re-enables initial spike artifacts)

---

## Priority Matrix

| Priority | Issue | Impact | Effort |
|----------|-------|--------|--------|
| P0 | Tune impulse_window_ms for HL tick rate | Enable fills | Low |
| P0 | Fix "No book" race condition | Enable fills | Medium |
| P1 | WebSocket reconnection | Production resilience | Medium |
| P1 | Live exchange book subscriptions | OBI live capability | High |
| P2 | Per-venue latency asymmetry | Realistic sim | Medium |
| P2 | SIMD lag search | Latency optimization | High |
| P3 | Prometheus metrics | Monitoring | Medium |
