# TokioParasite — Multi-Exchange Lead-Lag Arbitrage Engine

TokioParasite is a high-performance, low-latency HFT engine optimized for lead-lag arbitrage across multiple centralized and decentralized exchanges.

## 🚀 Key Features

*   **Multi-Exchange Support**: Seamlessly trade on **Hyperliquid**, **OKX**, and **MEXC**.
*   **Low-Latency Core**: <10µs hot-path latency with CPU pinning and zero-allocation processing.
*   **Lead-Lag Arbitrage**: Exploits price discovery latency between Binance Futures (Lead) and various Lag venues.
*   **MAKER Strategy**: Optimized for passive liquidity provision with automated take-profits and alpha decay exits.
*   **HFT Networking**: Mandatory `TCP_NODELAY` and manual socket management for minimum RTT.

## 🛠 Quick Start

### 1. Configuration
Edit `settings.toml` to select your trading mode and target exchange:

```toml
[app]
trading_mode = "paper"      # "paper" or "live"
target_exchange = "okx"     # "hyperliquid", "okx", or "mexc"
```

### 2. Run the Bot
```bash
cargo run --release
```

## 📖 Documentation

Detailed documentation is available in the [`docs/`](docs/) directory:

*   [**Architecture Overview**](docs/architecture.md) — System design and data flow.
*   [**Module Map**](docs/README.md) — Breakdown of internal modules (`eal`, `oms`, `signal`, etc.).
*   [**Configuration Guide**](docs/modules/config.md) — Tuning strategy parameters.
*   [**Changelog**](docs/CHANGELOG.md) — History of releases and fixes.

## ⚖️ License
Proprietary. All rights reserved.
