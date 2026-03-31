# TokioParasite Operations & Diagnostics Guide (How-To)

## 🚀 1. Starting the Bot

### Debug/Testing Mode (Local or VPS)
Run the bot with `RUST_LOG` set to `debug` to see detailed output, including raw ticks and book updates if needed. This is best for catching logic bugs or verifying the data feed.
```bash
# Export API keys if you intend to use live execution (for paper trading, dummy keys are fine)
export BINANCE_API_KEY="dummy"
export BINANCE_API_SECRET="dummy"
export HL_API_KEY="dummy"
export HL_API_SECRET="dummy"

# Run in debug mode (unoptimized, good for rapid iteration)
RUST_LOG=debug cargo run
```

### Production Mode (VPS)
Production mode requires a release build for maximum performance (critical for our sub-10µs latency target).
```bash
# 1. Pull latest changes
git pull origin main

# 2. Build in release mode
cargo build --release

# 3. Edit settings.toml to ensure it's ready for live data
# Make sure use_real_data = true
nano settings.toml

# 4. Run in background with nohup (keeps it running after SSH disconnect)
RUST_LOG=info nohup ./target/release/tokioparasite > bot.log 2>&1 &

# 5. Monitor the live log
tail -f bot.log
```

## 🛠️ 2. Diagnostics & Telemetry

### Checking Tick & Book Update Rates
The bot emits a `HEARTBEAT` every 10 seconds. This is your primary measure of system health and throughput.
```bash
grep "HEARTBEAT" bot.log | tail -n 20
```
*What to look for:*
- **Binance tick rate:** Should consistently be ~70-100 ticks/sec.
- **Hyperliquid tick rate:** Should be ~1-5 ticks/sec (HL only sends actual trades, not every book change as a tick).
- **L2 Book Rates:** Binance ~80/sec, HL ~4/sec.
- *Troubleshooting:* If these numbers drop significantly, it means WebSocket lag, disconnection, or severe throttling.

### Checking Latency to Exchanges (Network Layer)
To verify your AWS Tokyo edge is actually an edge:
```bash
# Simple ping to measure raw RTT (Round Trip Time)
ping fstream.binance.com
ping api.hyperliquid.xyz

# MTR for packet loss / routing diagnostics along the path
mtr --report -c 10 fstream.binance.com
```
*Optimal Tokyo Latency:* < 5ms to Binance Japan endpoints, < 15ms to Hyperliquid.

### Computational Overhead
To ensure our hot-path isn't blocking and we have spare CPU cycles:
```bash
# Use HTOP or TOP to see thread-level CPU usage
htop -p $(pgrep tokioparasite)
```
- A core pegged at 100% is normal if `perf_mode = true` is set, since the bot uses aggressive spin-looping/yielding to minimize context-switch latency.
- Watch the memory usage; if it climbs steadily without capping, it indicates a memory leak in the book state or telemetry vectors.

## 💰 3. Tracking Execution & PnL

### Checking Live Position Snapshot
The bot periodically logs the current open positions, execution stats, and daily PnL:
```bash
grep -A 10 "POSITIONS" bot.log | tail -n 30
```

### Finding Reject Reasons (Why didn't we trade?)
If the bot isn't placing orders, it is likely hitting protective risk gates. Use these to find out which one:
```bash
# Check if the $100 position cap is blocking trades
grep "Position cap" bot.log | wc -l

# Check for stale books (data older than our freshness gate)
grep "No book" bot.log

# Check if the side-aware cooldown prevented an entry
grep "Cooldown" bot.log
```

### Checking Alpha Decay (The "Edge")
The v0.2.0 engine uses Alpha Decay Probes. It measures the wall-clock time between the leader (Binance) moving and the laggard (Hyperliquid) catching up.
To pull this data and calculate optimal timeouts:
```bash
# Make sure your Python environment has pandas
# Run the analysis script against the telemetry file (or DB)
python3 analyze_decay.py
```
*Insight:* The output tells you exactly how many milliseconds it takes for Hyperliquid to converge. Take these numbers and update your `settings.toml`.

## 🔄 4. Hot-Reloading Configuration
As of v0.2.0, you **do not need to restart the bot** to tune strategy parameters, adjust exit timeouts, or change the `take_profit_bps`.
1. Open the configuration file: `nano settings.toml`
2. Change a value (e.g., `take_profit_bps = 10.0` to `take_profit_bps = 11.5`).
3. Save the file.
4. The bot's main heartbeat loop automatically checks the file modification time every 15 seconds. It will seamlessly propagate the new settings to the OMS and Signal handlers.
5. Check logs for confirmation: `grep "Reloaded strategy settings" bot.log`

## 🧰 5. Operations "Tricks"

### Find the Bot Process ID
```bash
pgrep -l tokioparasite
```

### Gracefully Kill the Bot
Avoid `kill -9` unless absolutely necessary. Use SIGTERM so the async runtime drops gracefully.
```bash
pkill -SIGTERM tokioparasite
```

### Manage Log Bloat
High-frequency logs grow rapidly. Verify size and flush them without killing the process:
```bash
# Check the log file size
ls -lh bot.log

# Truncate the log safely while the bot is running
> bot.log
```

### Tail Specific Pairs
If you are tweaking the parameters specifically for `DOGE` and want to watch just its flow:
```bash
tail -f bot.log | grep -i "DOGE"
```

### Watch the Log in Real-time (Colorized)
If you have `bat` installed, or just want clear separation:
```bash
tail -f bot.log | grep --color=auto "HEARTBEAT"
```
