## [0.3.1] — 2026-04-05 — Take-Profit & Fill Notification Fix
1: 
2: ### Fixes & Logic (The "Blindness" Patch)
3: - **PaperSimulator Synchronous Broadcast** — Fixed bug where IOC/Market orders bypassed the fill notification channel. OMS and Runners now correctly see all simulator fills.
4: - **Automated Take-Profit Restoration** — Restored immediate TP limit order generation on both paper and live runners.
5: - **Position State Synchronization** — OMS `NetDelta` now reflects filled positions in real-time, preventing state desync and incorrect position-cap rejections.
6: 
7: ---
8: 
9: ## [0.3.0] — 2026-04-05 — HFT Network Optimization & Multi-Runner Architecture
1: 
2: ### Performance & Network (The "Low-Latency" Push)
3: - **TCP_NODELAY Enforcement** — Mandatory for HFT. Disabled Nagle's Algorithm across all exchange connections (Binance, Hyperliquid) to eliminate 10–40ms of packet buffering latency.
4: - **Manual Handshake Optimization** — Substituted `tokio_tungstenite::connect_async` with manual `TcpStream` + `set_nodelay(true)` + `tokio_native_tls` for direct socket control.
5: - **Latency-Optimized REST Client** — Enabled `tcp_nodelay(true)` on `reqwest` clients to ensure authenticated order execution isn't delayed by Nagle.
6: 
7: ### Execution & Risk (Liquidity Management)
8: - **Liquidity-Aware Position Sizing** — OMS now caps order sizes to `best_level_size * fill_conservatism` (e.g., 50% of top-of-book). Prevents immediate price impact and slippage.
9: - **Consistent Symbol Normalization** — Unified normalization across `PaperSimulator`, `SignalPipeline`, and `OrderManagementSystem` to ensure perfect cross-venue ticker matching.
10: - **Minimum Notional Stability** — Enforced $10.1 USD minimum notional locally to satisfy Hyperliquid's exchange limits.
11: 
12: ### System Architecture (Maintenance)
13: - **Isolated Runner Model** — Split `runners::live` and `runners::paper` into distinct modules. Simplifies paper trading simulation vs real capital execution.
14: - **AWS Production Proofing** — Bot validated on Amazon Linux (graviton/x86). Verified cloud performance and connectivity stability.
15: 
16: ---
17: 
18: ## [0.2.0] — 2026-03-31 — Passive Market Making & Alpha Decay

### Execution Model (The "Maker" Shift)
- **Passive Limit Lifecycle** — Transitions from liquidity-taking (Market/IOC) to **Post-Only Limit** entries at mid-price.
- **Automated Take-Profit (TP)** — OMS now generates secondary limit orders at **+13.0 bps** upon entry fill.
- **Symbol-Specific Timeouts** — Tiered exit logic based on alpha decay: **1.0s** (Fast), **2.5s** (Slow), **1.5s** (Default).
- **Limit-Matching Simulator** — `PaperSimulator` rebuilt for asynchronous fill broadcasting and pending-limit order book matching.

### Strategy & Telemetry
- **Alpha Decay Probes** — High-resolution wall-clock instrumentation measures the Tokyo opportunity window (Avg: 500ms).
- **Configuration Hot-Reload** — 15-second filesystem watcher re-loads `settings.toml` live. Adjust thresholds/timeouts without process restarts.
- **Optimized Thresholds** — `entry_threshold_bps = 4.5`, `lag_threshold_bps = 1.0`, `obi_persist_ms = 30`.

### Infrastructure
- **Symbol Normalization (v2)** — Unified `Symbol::normalize()` across all metadata/matchers.
- **OMS Pending Trackers** — Enhanced position tracking by fill-timestamp for better risk accounting during execution lag.

---

## [0.1.5] — 2026-03-31 — Signal Quality and Position Management Enhancements

### Entry Logic
- **Freshness gate 400ms** — Both venues must have ticked within 400ms (local timestamps). Exchange timestamps unreliable across venues.
- **Lag check restored** — `other_is_lagging` checks `|delta| < 1.5 bps` (was hardcoded `true` in v0.1.2).
- **Fees-aware spread check** — `entry_threshold_bps = 8` (covers taker fees + slippage). Direction-normalized edge: `edge_bps = (target - source) / source * 10000` with buy/sell logic.
- **Combo window 150ms** — `signal_timeout_ms` increased from 10ms to 150ms.
- **TTL 500ms** — Signal time-to-live reduced from 1500ms to 500ms.
- **400ms book age gate** — Hard reject if target venue book > 400ms stale.

### Execution Model
- **Side-aware cooldown** — `(symbol, side)` key in OMS, 200ms between trades. Allows valid reversals (BUY then SELL on same symbol).
- **Conservative fill** — `allowed_size = best_level_size * 0.5`. Only fill half of what's visible. Real books shift during latency.
- **$100 position cap** — Cumulative notional per `(venue, symbol)` capped at $100. Direction-aware: LONG can accept SHORT to reduce, but not more LONG. `max_notional_usd` reduced from $5,000 to $10.
- **`best_bid_size()` / `best_ask_size()`** — Added to `OrderBookMatcher` for book consumption model.

### Signal Processing
- **Weighted OBI** — Depth-weighted: `weight = 1/(i+1)`. Top levels dominate over deep levels.
- **Time-based OBI persistence** — OBI must stay strong for 200ms. Not count-based (which varies by venue: Binance 80/sec vs HL 4/sec).
- **Local timestamps for freshness** — `MidpriceTracker.last_local_update_ns` uses `SystemTime::now()`. Exchange timestamps only used for delta calculation (consistent within venue).

### Infrastructure
- **Per-symbol performance tracking** — Heartbeat shows `ZEC: 0/0 (0%) | WLD: 5/12 (42%)` fill/reject rates.
- **158 tests passing** — New tests: weighted OBI calculation, OBI persistence filter, local timestamp tracking.

---

## [0.1.2] — 2026-03-29 — Real L2 Order Books & Per-Venue Model

### Architecture
- Real L2 order book subscriptions (Binance `@depth@100ms`, Hyperliquid `l2Book`)
- `LocalOrderBook` with BTreeMap-based bid/ask state for Binance diff stream
- Per-venue spread model (Binance: 1 bps, Hyperliquid: 5 bps)
- Symbol normalization (`ZECUSDT` → `ZEC`) for cross-venue keying
- Staleness tracking per `(Symbol, VenueId)`
- `SimMetrics` for fresh/stale/no_book tracking

### Bug Fix
- Hyperliquid WebSocket `l2Book` channel parsing alongside `trades`
- Book receiver loop fixed (was overwriting per symbol)
- `simulate_fill` uses `get_mut()` instead of `entry().or_insert_with()`

---

## [0.1.1] — 2026-03-28 — Plan Review Fixes

- Impulse-OBI wired into main loop
- PaperSimulator replaces MockExchange
- ImpulseDetector cross-venue tracker pollution fixed
- OMS `ExecutionFailed` error variant

---

## [0.1.0] — 2026-03-26 — Initial Release

- Modular architecture (EAL, Signal, OMS, Sim, Persist)
- Incremental Pearson correlation, zero hot-path allocations
- 132 tests passing

---

## [Unreleased]
- WebSocket reconnection with state recovery
- Live exchange `subscribe_book()` implementations
- Prometheus metrics
- Binance diff stream gap re-sync
- Per-venue latency asymmetry modeling
