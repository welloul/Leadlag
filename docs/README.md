# TokioParasite — Lead-Lag Arbitrage Engine (v0.6.0)

## System Architecture

```mermaid
graph TB
    subgraph "Async I/O Zone"
        WS_Lead[Binance Futures<br/>Lead Venue]
        WS_Lag[HL/OKX/MEXC<br/>Lag Venues]
        WS_Lead -->|Arc<Tick>| CH_A[Channel A]
        WS_Lag -->|Arc<Tick>| CH_B[Channel B]
    end

    subgraph "Sync Hot Path (CPU-Pinned)"
        CH_A -->|crossbeam| HP[Hot Path Thread]
        CH_B -->|crossbeam| HP
        HP --> SIG[Signal Generation<br/>Impulse + OBI]
    end

    subgraph "Async OMS Zone"
        SIG --> OMS[Order Management<br/>System]
        OMS --> EX[Execution<br/>Engine]
        EX -->|EAL| VEN[Target Venue<br/>REST/WS API]
    end
```

## Module Map

| Module | Purpose | Venues |
|--------|---------|--------|
| `eal` | Exchange Abstraction Layer | Binance, HL, OKX, MEXC |
| `runners` | Isolated execution loops | Paper, HL, OKX, MEXC |
| `signal` | Hot-path signal pipeline | Logic-unified |
| `oms` | Risk, CD, & Order routing | Unified gating |

## Documentation Index

- [**Architecture Overview**](docs/architecture.md) — Multi-exchange data flow.
- [**Runners & Execution Modes**](docs/modules/runners.md) — v0.6.0 runner model.
- [**Changelog**](docs/CHANGELOG.md) — Detailed version history.