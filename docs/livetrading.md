# TokioParasite: Live Trading Implementation Plan (Hyperliquid)

## 🎯 Objective
Transition the TokioParasite bot from `PaperSimulator` to live asynchronous execution exclusively on **Hyperliquid**. The implementation adheres to **ultra-low-latency** and **non-blocking** async patterns to preserve the strategy's competitive edge.

## 🏗️ Architectural Overview
The bot's core execution flow relies on the `OrderExecution` trait within the Exchange Abstraction Layer (EAL).
The `HyperliquidLiveExecutor` handles real-world connectivity, cryptographic signing, and order synchronization.

## ✅ Completed Fixes (v0.2.0)

### 1. Boot-Time Meta State Sync
*   **Problem**: Hyperliquid L1 transactions require a numeric `asset_index`. Hardcoding these is dangerous as the universe changes.
*   **Solution**: On startup, the executor halts and calls `load_asset_context()`. It fetches the `meta` state from `/info`, maps symbol names (e.g., "DOGE") to canonical indexes, and verifies trades against this live map.
*   **Safety**: If a symbol is missing from the Meta State but exists in our config, the bot triggers a **FATAL ABORT** to prevent invalid signature crashes.

### 2. Entry & Partial Fills (The "Double TP" Fix)
*   **Problem**: `process_fill()` previously sized Take-Profits based on total `cumulative_size`. Partial fills would cause the bot to sprout multiple full-sized TP orders, creating massive over-exposure.
*   **Solution**: TP orders are now sized exactly to `fill.filled_size`. The logic explicitly identifies `is_entry` bounds to ensure exits don't trigger recursive TP spawns.

### 3. Missing `reduce_only` Safeties
*   **Problem**: `OrderRequest` lacked a `reduce_only` flag. Resting TP limits survived position closures, creating "orphaned" orders that could force the bot into reverse positions.
*   **Solution**: Added `reduce_only` to the core `OrderRequest` struct. All TP and Time-Exit orders are strictly flagged as `true`. Hyperliquid's matching engine now automatically purges these orders the moment a position is flattened.

### 4. Dynamic Exposure Calculation (The "Memory Leak" Fix)
*   **Problem**: Brittle `HashMap` trackers (`cumulative_size`, etc.) could desync from the exchange if an order was rejected or a fill was dropped.
*   **Solution**: Ripped out static trackers. The OMS now calculates exposure **dynamically** in every check:
    `Exposure = net_delta.position (FILLED) + pending_orders.sum (INTENT)`.
*   **Internal Ripple**: `check_time_exits()` now iterates directly over `net_delta.positions()`, using the native physical fill timestamp for precise timing.

---

## 📋 Operational Workflow

### 1. Initialization
1.  Loads `HL_API_KEY` and `HL_API_SECRET` from environment.
2.  Fetches Global Meta State (Authoritative Symbol Mappings).
3.  Syncs Clearinghouse State (Initial Positions).

### 2. Entry Path (Maker-First)
1.  **Signal**: Signal Pipeline detects Lead-Lag impulse.
2.  **Order**: OMS generates a `Post-Only` Limit Order at mid-price.
3.  **Submission**: Detached async task signs EIP-712 payload and POSTs to exchange.
4.  **Pending**: Order resides in `pending_orders` map, consuming risk cap.

### 3. Fill & TP Path
1.  **Event**: WS User Stream pushes `FillEvent`.
2.  **Update**: `net_delta` updates physical position; `pending_orders` decays by filled amount.
3.  **TP**: OMS immediately triggers a `reduce_only` TP Limit at +13 bps.

### 4. Exit Path (Emergency/Time)
1.  **Trigger**: `check_time_exits()` detects a position held beyond `exit_timeout_ms`.
2.  **Order**: OMS generates a `reduce_only` IOC Market order.
3.  **Cleanup**: Resting TP orders are automatically invalidated by the exchange's `reduce_only` engine.

---

## 🛡️ Risk Controls
1.  **Hardware Firewall**: Hardcoded `50.0 USD` max notional limit in `hyperliquid_exec.rs` (independent of config).
2.  **Post-Only Enforcement**: Rejects entry orders that would require taking liquidity.
3.  **Dynamic Sync**: State is recalculated from physical fills to prevent "Stuck Bot" memory leaks.

*Status: **LIVE READY**. v0.2.0 deployed and rsynced to AWS.*
