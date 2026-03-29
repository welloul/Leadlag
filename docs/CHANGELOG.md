# Changelog

All notable changes to TokioParasite are documented here.

Format: `[Version] — Date — Category`

---

## [0.1.2] — 2026-03-29 — AWS Deployment & Execution Model

### Architecture
- **Per-venue order books** — `PaperSimulator` matchers keyed by `(Symbol, VenueId)` instead of `Symbol` alone. Each venue maintains independent L2 books. This is non-negotiable for cross-venue lead-lag execution.
- **Per-venue spread model** — `VenueSpreadModel` with base spread (Binance: 1 bps, Hyperliquid: 5 bps) and size impact factor. Reflects real liquidity differences between venues.
- **Correct execution price** — `oms.process_signal()` receives target venue's mid price via `simulator.get_mid_price()`, not source tick's price. Cross-venue price sourcing was a critical correctness bug.
- **Cross-venue book seeding** — When only one exchange sends data, the other venue's book is seeded from the first tick's price. Ensures both venues are tradeable in asymmetric data scenarios.
- **`simulate_fill` uses `get_mut`** — Replaced `entry().or_insert_with()` with `get_mut()`. Silently creating empty matchers was hiding venue-isolation bugs.

### Bug Fix
- **Hyperliquid WebSocket parsing** — Response format is `{"channel":"trades","data":[...]}`, not raw arrays. Parser now handles channel-wrapped messages, subscription confirmations, and connection errors.
- **Impulse warmup gate** — Both trackers must be `initialized` AND `warmed_up` before generating impulses. Prevents 382k bps initialization spike artifacts when one venue has no data yet.
- **Impulse sanity check** — Deltas > 500 bps silently rejected. Real microstructure impulses are 5-50 bps. Large deltas indicate stale initialization prices or data errors.
- **`other_is_lagging` fix** — `None` from `other_delta()` means "no data yet" and is treated as NOT lagging (was incorrectly treated as lagging, causing false signals).
- **Correlation check skipped for Impulse-OBI** — `check_correlation()` returns `Ok(())` when `active_strategy == "impulse_obi"`. Impulse-OBI uses its own conviction scoring, not Pearson correlation.
- **OMS swallowed execution errors** — `process_signal()` now uses `RiskError::ExecutionFailed(String)` instead of misleading `ExceedsMaxNotional { 0.0, 0.0 }`.

### Infrastructure
- **AWS deployment** — Bot deployed on EC2 (13.231.81.63, Amazon Linux 2023, ap-northeast-1). Rust 1.94.1, release build.
- **Heartbeat logging** — Per-venue tick counters with 5-second heartbeat for debugging data flow asymmetry.

### Tests
- **144 tests passing** — Added per-venue isolation tests, impulse warmup tests, sanity check tests.
- **`test_per_venue_isolation`** — Verifies Exchange B order fails when only A has a book.
- **`test_per_venue_different_prices`** — Verifies fills reflect independent venue pricing.
- **`test_is_venue_liquid`** — Verifies liquidity readiness check.
- **`test_impulse_skipped_before_both_initialized`** — Verifies warmup gate.
- **`test_midprice_tracker_basic`** — Updated for warmup behavior.

### Known Issues Discovered on AWS
1. **Low fill rate with Hyperliquid** — Hyperliquid sends ~1 tick/sec vs Binance's ~18/sec. The `other_is_lagging` check requires both venues to have recent deltas within the 5ms window, which rarely happens with HL's slow tick rate.
2. **"No book" errors** — Race condition between book seeding and signal generation. The target venue's book may not be populated when an impulse signal tries to execute.
3. **ZEC/LINK initial price delta** — These symbols have massive initial price differences between venues (9606 bps, 244k bps). The warmup+sanity check handles this but limits signal generation for these symbols.

---

## [0.1.1] — 2026-03-28 — Plan Review Fixes

### Architecture
- Impulse-OBI wired into main loop
- Book subscriptions added
- PaperSimulator replaces MockExchange
- Duplicate TimeGrid removed

### Bug Fix
- ImpulseDetector cross-venue tracker pollution
- NaN/Inf in MidpriceTracker
- OMS swallowed execution errors
- Integration tests non-compiling
- Hysteresis test misleading

### Allocation Changes
- Eliminated signal clone via `PendingSignal`
- `saturating_sub` for timeout

### GC Pressure
- `yield_now()` replaces `sleep(100µs)`

### Tests
- 4 new Impulse-OBI tests
- 137 total tests

---

## [0.1.0] — 2026-03-26 — Initial Release

- Complete modular architecture (EAL, Signal, OMS, Sim, Persist)
- Incremental Pearson correlation with O(1) running sums
- Zero allocations on hot path
- Paper trading simulator with L2 matching
- 132 tests passing

---

## [Unreleased]

### Planned
- WebSocket reconnection with state recovery
- Live exchange `subscribe_book()` implementations
- Prometheus metrics export
- Per-venue latency asymmetry modeling
- Liquidity decay for thin books
