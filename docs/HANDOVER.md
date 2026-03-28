# Handover Manual — Operations & Recovery

## Project Status

| Aspect | Status |
|--------|--------|
| **Phase** | Paper Trading (v0.1.0) |
| **Stability** | All 60+ tests passing (49 unit + 7 integration + 4 signal flow) |
| **Live Trading** | Disabled (simulation only) |
| **API Keys** | Hyperliquid only (Binance pending) |

## Context for Next AI

### DO NOT CHANGE
1. **Ring buffer size** — Must remain power of 2 (currently 256)
2. **Hot path thread model** — Dedicated OS thread with spin-loop
3. **Crossbeam channels** — Do not replace with tokio::sync::mpsc
4. **Atomic kill switch** — Must use SeqCst ordering for safety

### KNOWN HACKS
1. `TimeGrid::ingest_tick` returns `Vec<AlignedPair>` — allocates on heap per tick. Should be pre-allocated.
2. `NetDelta` uses `HashMap<(VenueId, Symbol), Position>` — O(n) lookup. Should use fixed-size array.
3. `PreflightChecker::check_max_slippage` ignores `current_price` parameter — placeholder logic.

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

### settings.toml Key Fields

```toml
[app]
cpu_pinning = true      # Set false for macOS development
perf_mode = true        # Enables spin-loop (100% CPU)

[simulation]
enabled = true          # Paper trading (MUST be true until validated)

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
| `Signal generated` | Trade opportunity | Check OMS logs |
| `Connection lost` | Exchange down | Check network |

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