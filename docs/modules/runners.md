# Runners Module — Execution Orchestration (v0.6.1)

## Objective
Decouple trading venues and execution environments into isolated event loops. Each runner owns its exchange connections, signal pipeline, and OMS instance.

## Architecture
`src/main.rs` reads `settings.toml` and routes to the appropriate runner based on `trading_mode` and `target_exchange`.

### Runner Selection (v0.6.0+)
```toml
[app]
trading_mode = "paper"         # "paper" | "live"
target_exchange = "okx"        # "hyperliquid" | "okx" | "mexc"
```

| trading_mode | target_exchange | Runner |
|---|---|---|
| `paper` | `okx` | `runners::okx` (paper path via `PaperSimulator`) |
| `paper` | `mexc` | `runners::mexc` (paper path) |
| `paper` | `hyperliquid` | `runners::hyperliquid` (paper path) |
| `live` | `okx` | `runners::okx` (live path via `OkxLiveExecutor`) |
| `live` | `mexc` | `runners::mexc` (live path via `MexcLiveExecutor`) |
| `live` | `hyperliquid` | `runners::hyperliquid` (live path via `HyperliquidLiveExecutor`) |

> **Binance is always Exchange A (lead).** The `target_exchange` sets Exchange B (the laggard we trade).

## Runner Isolation

### `runners::hyperliquid`
- **Data**: Binance (lead) + Hyperliquid (lag)
- **Paper**: `PaperSimulator` for order matching
- **Live**: `HyperliquidLiveExecutor` — EIP-712 signing, boot-time Meta State sync, CLOID eviction
- **Safety**: Ghost-CLOID eviction from rejected orders; leverage sync on startup

### `runners::okx`
- **Data**: Binance (lead) + OKX (lag)
- **Paper**: `PaperSimulator` for fills
- **Live**: `OkxLiveExecutor` — HMAC-SHA256 REST V5
- **Book channel**: OKX `books5` (5-level) for OBI detection

### `runners::mexc`
- **Data**: Binance (lead) + MEXC (lag)
- **Paper**: `PaperSimulator` for fills
- **Live**: `MexcLiveExecutor` — HMAC-SHA256 REST Futures

### `runners::paper` (legacy)
- Uses Binance + Hyperliquid data feeds with PaperSimulator only
- Kept for backward compatibility; prefer `runners::hyperliquid` with `trading_mode = "paper"`

## Core Loop Sequence (all runners)

1. **Ingest**: Receive `Arc<Tick>` / `Arc<BookUpdate>` from exchange channels
2. **Process**:
   - Update simulator book (`simulator.update_book_from_tick` / `update_book`)
   - Feed into `SignalPipeline::process_tick`, `process_book`, `process_book_for_impulse`
3. **Signal**: If signal generated → check book staleness, get exec price
4. **Execute**:
   - Paper path: `oms.process_signal(&signal, exec_price, &simulator)`
   - Live path: `oms.process_signal(&signal, exec_price, &live_executor)`
5. **Monitor** (every 500ms):
   - `oms.check_pending_ttl()` — cancel TTL-expired orders
   - `oms.check_time_exits()` — close positions past `exit_timeout_ms`
6. **Heartbeat** (every 5s): tick counts, signal count, open positions, per-symbol fill/reject rates
7. **Config Hot-Reload** (every 15s): re-reads `settings.toml`, updates `pipeline` and `oms` in-place

## Alpha Decay Probes
All runners instrument edge decay: on each impulse signal, a `DecayProbe` is inserted. When the laggard price catches the leader's target price, elapsed ms is logged as `ALPHA_DECAY`.

## Module Layout
```
src/runners/
├── mod.rs           — pub use declarations
├── hyperliquid.rs   — Hyperliquid runner (paper + live)
├── okx.rs           — OKX runner (paper + live)
├── mexc.rs          — MEXC runner (paper + live)
└── paper.rs         — Legacy paper-only runner
```
