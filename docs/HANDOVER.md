# Handover Manual — Operations & Recovery

## Project Status

| Aspect | Status |
|--------|--------|
| **Phase** | Paper Trading (v0.1.2) |
| **Stability** | All 144 tests passing |
| **Live Trading** | Disabled (simulation only) |
| **API Keys** | Not required (market data is public) |
| **Active Strategy** | Impulse-OBI (both exchanges live on AWS) |
| **Deployment** | AWS EC2 (13.231.81.63, Amazon Linux 2023, ap-northeast-1) |

## Context for Next AI

### DO NOT CHANGE
1. **Ring buffer size** — Must remain power of 2 (currently 256)
2. **Hot path thread model** — Dedicated OS thread with spin-loop
3. **Crossbeam channels** — Do not replace with tokio::sync::mpsc
4. **Atomic kill switch** — Must use SeqCst ordering for safety
5. **ImpulseDetector venue routing** — Ticks must be routed to the correct tracker (tracker_a for Exchange A, tracker_b for Exchange B). Cross-pollution was a critical bug.
6. **MidpriceTracker warmup** — Both trackers must be `initialized` AND `warmed_up` before generating impulses. Removing this causes massive false spike impulses (382k bps artifacts).
7. **Per-venue simulator keying** — Matchers keyed by `(Symbol, VenueId)`. Each venue has independent order books. Reverting to `Symbol`-only keying destroys cross-venue execution.

### KNOWN HACKS
1. `PreflightChecker::check_max_slippage` uses a size-impact model based on order notional — approximate but sufficient for paper trading.
2. **Book subscriptions not available for live exchanges** — `BinanceExchange::subscribe_book()` and `HyperliquidExchange::subscribe_book()` return `Err(Not implemented)`. The PaperSimulator synthesizes books from tick prices instead.
3. **Main loop uses `tokio::task::yield_now()`** instead of `std::hint::spin_loop()` — the async runtime requires cooperative scheduling.
4. **Impulse sanity check** — Deltas > 500 bps are rejected as initialization artifacts. Real microstructure impulses are 5-50 bps.
5. **Other-venue book seeding** — When only one exchange sends data, the other venue's book is seeded from the first tick's price. This is a fallback for asymmetric data feeds.
6. **`other_is_lagging = false` when `None`** — If the other tracker has data but no delta yet (window hasn't elapsed), we conservatively treat it as NOT lagging. This prevents premature signals but reduces fill rate when one venue ticks slowly.

### RECENT FIXES (v0.1.2 — AWS Deployment)
1. **Hyperliquid WebSocket parsing fixed** — Response format is `{"channel":"trades","data":[...]}`, not raw arrays. Parser now handles channel-wrapped messages and subscription confirmations.
2. **Per-venue order books** — `PaperSimulator` matchers keyed by `(Symbol, VenueId)` instead of `Symbol` alone. Each venue has independent L2 books with per-venue spread models (Binance: 1 bps base, Hyperliquid: 5 bps base).
3. **Correct execution price** — `oms.process_signal()` now receives target venue's mid price via `simulator.get_mid_price()`, not source tick's price.
4. **Impulse warmup gate** — Both trackers must be `initialized` (received at least 1 tick) AND `warmed_up` (completed one full window cycle) before generating impulses.
5. **Impulse sanity check** — Deltas > 500 bps are silently rejected. Eliminates 382k bps initialization spike artifacts.
6. **`other_is_lagging` fix** — `None` from `other_delta()` now means "no data yet" and is treated as NOT lagging (was incorrectly treated as lagging).
7. **`simulate_fill` uses `get_mut`** — Replaced `entry().or_insert_with()` with `get_mut()`. Silently creating empty matchers was hiding venue-isolation bugs.
8. **Per-venue spread model** — `VenueSpreadModel` with base spread (Binance: 1 bps, Hyperliquid: 5 bps) and size impact factor.
9. **Heartbeat logging** — Per-venue tick counters with 5-second heartbeat for debugging data flow.
10. **Correlation check skipped for Impulse-OBI** — `check_correlation()` returns `Ok(())` when `active_strategy == "impulse_obi"`.

### RECENT FIXES (v0.1.1 — Plan Review)
1. Impulse-OBI wired into main loop with `process_tick()` and `process_book()`.
2. Book subscriptions added for OBI strategy.
3. PaperSimulator replaces MockExchange.
4. Duplicate TimeGrid removed.
5. ImpulseDetector cross-venue tracker pollution fixed.
6. NaN/Inf guards in MidpriceTracker.
7. OMS error handling: `RiskError::ExecutionFailed(String)`.
8. Integration tests fixed.
9. Hysteresis test corrected.
10. Signal clone eliminated via `PendingSignal`.

## Recovery Procedures

### AWS Deployment

```bash
# SSH into instance
ssh -i ~/Desktop/tokio.pem ec2-user@13.231.81.63

# Check if bot is running
pgrep tokioparasite

# View live logs
tail -f ~/bot.log

# Restart bot
kill $(pgrep tokioparasite)
cd ~/tokioparasite
./target/release/tokioparasite > ~/bot.log 2>&1 &
disown

# Rebuild after code changes
source ~/.cargo/env
cargo build --release
```

### Local Development

```bash
# Run with mock exchanges (safe)
RUST_LOG=tokioparasite=info cargo run

# Run with real market data
# Set use_real_data = true in settings.toml first
RUST_LOG=tokioparasite=info cargo run

# Run tests
cargo test
```

## Configuration Reference

### settings.toml Key Fields

```toml
[strategy]
active_strategy = "impulse_obi"
symbols = ["ZEC", "WLD", "FARTCOIN", "DOGE", "SUI", "BCH", "PUMP", "ADA"]

# Impulse-OBI settings
impulse_threshold_bps = 5        # 5 bps price move to detect impulse
lag_threshold_bps = 1.5          # Max move on other exchange to consider it lagging
impulse_window_ms = 5            # 5ms lookback for price change
signal_timeout_ms = 10           # Signal expiry
min_trade_size_filter = 0.001    # Filter fake impulses from small trades
spread_filter_bps = 10           # Max spread to trade

[simulation]
enabled = true
use_real_data = true             # Live Binance + Hyperliquid WebSocket data
latency_simulation_ms = 5
fee_tier_bps = 2.5
match_l2_depth = 10

[risk]
max_notional_usd = 5000.0
max_drawdown_daily = 200.0
signal_ttl_ms = 150
```

## Monitoring

### Key Log Messages

| Message | Meaning | Action |
|---------|---------|--------|
| `HEARTBEAT` | Per-venue tick counters | Monitor data flow balance |
| `Impulse detected` | Price move detected (5-50 bps range) | Check lagging status |
| `Laggard confirmed` | Other venue is flat → trade signal | Order should submit |
| `Order submitted` | Fill confirmed | Check positions |
| `Order rejected: Self-trade` | Overlapping order on same venue/symbol | Expected behavior |
| `POSITIONS` | Periodic PnL snapshot | Monitor profitability |
| `Hyperliquid WS error` | HL connection dropped | Check reconnection |

### AWS Status Commands

```bash
# Check bot status
ssh -i ~/Desktop/tokio.pem ec2-user@13.231.81.63 "pgrep tokioparasite && echo running"

# View heartbeat
ssh -i ~/Desktop/tokio.pem ec2-user@13.231.81.63 "grep HEARTBEAT ~/bot.log | tail -3"

# View positions
ssh -i ~/Desktop/tokio.pem ec2-user@13.231.81.63 "grep -A10 POSITIONS ~/bot.log | tail -12"

# Count fills vs rejections
ssh -i ~/Desktop/tokio.pem ec2-user@13.231.81.63 "echo -n 'Submitted: '; grep 'Order submitted' ~/bot.log | wc -l; echo -n 'Rejected: '; grep 'Order rejected' ~/bot.log | wc -l"
```
