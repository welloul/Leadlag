# Environment Setup & Infrastructure

## Infrastructure

### AWS Deployment
- **Instance**: t3.micro (2 vCPU, 1GB RAM) — sufficient for single-strategy HFT
- **OS**: Amazon Linux 2023 (kernel 6.1, systemd-based)
- **Region**: ap-northeast-1 (Tokyo) — minimizes latency to exchanges
- **Storage**: 20GB gp3 SSD, 3000 IOPS
- **Networking**: Default VPC, security group allows SSH (22) and WebSocket (443)
- **Cost**: ~$10/month (reserved instance)

### Local Development
- **OS**: macOS 13+ or Ubuntu 22.04+
- **CPU**: 4+ cores recommended for parallel testing
- **RAM**: 8GB minimum, 16GB for full simulation
- **Network**: Stable internet for WebSocket connections

## Dependencies

### Core Runtime
- **Rust**: 1.70+ (edition 2021)
  - `tokio` 1.0+ (async runtime, single-threaded for HFT)
  - `crossbeam` 0.8+ (bounded channels for backpressure)
  - `sled` 0.34+ (embedded DB for telemetry)
  - `tracing` 0.1+ (structured logging)

### Build Tools
- **Cargo**: Package manager and build system
- `cargo build --release` for optimized binaries
- `cargo test` for unit/integration tests

### Exchange APIs
- **Binance**: Public WebSocket streams (no API key required)
  - `@trade` for ticks, `@depth@100ms` for L2 books
- **Hyperliquid**: Public WebSocket streams
  - `trades` for ticks, `l2Book` for L2 books

## Latency-Critical Configurations

### Memory Layout
- **Ring buffers**: Size 256 (power of 2, fits in L1 cache)
- **Channel bounds**: crossbeam::bounded(1024) prevents memory bloat
- **Order books**: BTreeMap with depth limit (avoids unbounded growth)
- **Signal structs**: Copy-friendly (17 bytes), no heap allocation

### Async Runtime
- **Tokio flavor**: current_thread (single-threaded for minimal overhead)
- **Task spawning**: Avoid unless necessary (main loop is hot path)
- **Channel strategy**: Synchronous crossbeam over async tokio::mpsc

### Timing
- **Clocks**: Wall-clock (`SystemTime::now()`) for freshness gates
- **Timeouts**: 250ms signal window, 30ms OBI persistence
- **Cooldowns**: 200ms per (symbol, side) to prevent oscillation

### Networking
- **WebSocket**: Single connection per venue (multiplexed streams)
- **Reconnection**: Exponential backoff (1s to 60s)
- **Message parsing**: Zero-copy where possible (Arc<Tick> sharing)

## Deployment Steps

### Initial Setup
```bash
# Provision EC2 instance
aws ec2 run-instances --image-id ami-0abcdef1234567890 --instance-type t3.micro --key-name my-key

# SSH and install dependencies
ssh -i ~/.ssh/my-key.pem ec2-user@<instance-ip>
sudo yum update -y
sudo yum install -y git gcc

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Clone and build
git clone https://github.com/your-org/tokioparasite.git
cd tokioparasite
cargo build --release
```

### Configuration
```toml
# settings.toml
[strategy]
active_strategy = "impulse_obi"
symbols = ["BTC", "ETH"]  # Add symbols as needed

[risk]
max_notional_usd = 10.0
```

### Startup
```bash
# Run in background
./target/release/tokioparasite > bot_debug.log 2>&1 &
disown

# Monitor
tail -f bot_debug.log
```

### Monitoring
- **Logs**: Structured tracing output with HEARTBEAT, POSITIONS, SYMBOLS
- **Metrics**: Per-symbol fill rates, PnL tracking
- **Alerts**: Position cap hits, book gaps, high slippage

## Troubleshooting

### Common Issues
- **Stale data**: Check venue freshness in logs (< 400ms)
- **No signals**: Verify parameter thresholds vs market conditions
- **Memory usage**: Monitor BTreeMap sizes, add depth limits if needed
- **Network drops**: Check WebSocket reconnection logs

### Performance Tuning
- **CPU**: Profile with `perf` for hot path optimization
- **Memory**: Use heaptrack for allocation tracking
- **Latency**: Add tracing spans for component timing