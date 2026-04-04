# Runners Module — Execution Orchestration

## Objective
Decouple simulated/paper trading from live execution to prevent logic bleed and ensure safety. The `runners` module provides isolated event loops for different execution environments.

## Architecture
The bot uses a "Runner" pattern where `src/main.rs` acts as a thin bootstrapper that directs execution to either the `Paper` or `Live` runner based on configuration.

### Runner Selection
Selected in `settings.toml`:
```toml
[simulation]
enabled = true  # Boots runners::paper
# enabled = false # Boots runners::live
```

## Runner Isolation

### `runners::paper` (Paper Trading)
- **Environment**: No API keys required. Does not attempt to connect to authenticated Hyperliquid endpoints.
- **Execution**: Uses `PaperSimulator` exclusively for order matching.
- **Data**: Can use real market data feeds (Binance/Hyperliquid) or mock data.
- **State**: Does not sync live positions/balances from exchanges.

### `runners::live` (Live Execution)
- **Environment**: Requires `HL_API_KEY` and `HL_API_SECRET`. Performs EIP-712 signing.
- **Execution**: Uses `HyperliquidLiveExecutor` for real-capital deployment.
- **Safety**: 
    - Retains a `PaperSimulator` instance **only** as a local L2 OrderBook replica for low-latency mid-price queries.
    - Explicitly drains and evicts rejected client order IDs (CLOIDs) from the OMS pending map.
- **State**: Performs boot-time position and meta-state synchronization with Hyperliquid.

## Core Loop Sequence
Both runners share a similar event-loop structure but differ in their execution backends:

1. **Ingest**: Receive ticks/books from exchange channels.
2. **Process**: Pipeline updates (Correlation, Impulse detection, OBI).
3. **Signal**: If a signal is generated, calculate order parameters.
4. **Execute**: 
    - `paper`: Route to `simulator.submit_order()`.
    - `live`: Route to `live_executor.submit_order()`.
5. **Monitor**: 
    - Check time-based exits.
    - Process fills and submit Take-Profit orders.
    - Heartbeat and Config hot-reload (every 15s).

## Module Layout
- `src/runners/mod.rs`: Module definitions.
- `src/runners/paper.rs`: Implementation of the paper trading loop.
- `src/runners/live.rs`: Implementation of the live trading loop.
