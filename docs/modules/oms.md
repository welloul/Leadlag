# OMS Module — Order Management System (v0.3.0)

## Objective
Validate trade signals against risk limits, track positions, and execute orders. Implements position cap, side-aware cooldown, and **liquidity-aware sizing**.
Validate trade signals against risk limits, manage a passive market-making lifecycle, and automate exits. Implements `Post-Only` entries, automated Take-Profit limit orders, tiered symbol-specific time exits, and **order book depth capping**.

## Invariants

1. **Non-bypassable checks**: All 6 preflight checks must pass
2. **Kill switch atomic**: `AtomicBool` with `SeqCst` ordering
3. **No blocking**: OMS never blocks on DB writes
4. **Self-trade prevention**: Cannot submit opposite side to same venue/symbol
5. **Error propagation**: Execution errors wrapped in `RiskError::ExecutionFailed`
6. **Side-aware cooldown**: `(symbol, side)` key, 200ms between trades. Allows reversals.
7. **Position cap (Dynamic)**: $100 cumulative notional per `(venue, symbol)`. Calculated dynamically as `Filled + Pending`.
8. **Maker Priority**: Entries MUST be `Post-Only`. Rejects if matching engine would cross the spread.
9. **TP Coupling (Partial fill aware)**: Every entry fill MUST trigger a secondary Take-Profit limit order sized exactly to `fill.filled_size`.
10. **Reduce-Only Enforcement**: All TP and Time-Exit orders are flagged as `reduce_only` to prevent accidental position reversal.
11. **Liquidity Capping**: Orders are dynamically capped based on the available depth at the best level of the target venue. `filled_size = min(mag_size, best_level_size * fill_conservatism)`.

## Order Flow (v0.2.0 — Maker Mode)

```
TradeSignal ──▶ process_signal()
                │
                ├─ [1] Side-aware cooldown check
                ├─ [2] Preflight checks (Kill-switch, Daily Loss, TTL, Notional)
                ├─ [3] Maker-Price Calculation (Mid-price)
                ├─ [4] Submit Post-Only Limit Order
                └─ [5] Add to 'pending_orders' (awaits fill_rx)

fill_rx ──────▶ process_fill()
                │
                ├─ [1] Update pending order (decrement `size` by `filled_size`)
                ├─ [2] Update net position in `net_delta`
                └─ [3] GENERATE AUTOMATED TP:
                       Submit 'reduce_only' Limit Order sized to `fill.filled_size`

Tick/Book ────▶ check_time_exits()
                │
                ├─ [1] Iterate over `net_delta.positions()`
                ├─ [2] Lookup symbol_timeouts[symbol] or default
                ├─ [3] Calculate position age (now - pos.timestamp_ns)
                └─ [4] IF age > timeout: Submit 'reduce_only' IOC Exit Order
```

## Position Cap & Exposure Calculation
The OMS has moved away from brittle static state maps to **Dynamic Exposure Calculation**.

```rust
// Calculation logic used in process_signal:
let filled_size = net_delta.position_size(venue, symbol);
let pending_size = pending_orders.values()
    .filter(|o| o.symbol == symbol && o.venue == venue)
    .map(|o| if o.side == Buy { o.size } else { -o.size })
    .sum();

let current_size = filled_size + pending_size;
let current_notional = current_size.abs() * current_price;
```

This prevents memory leaks where a missed fill or rejected order would permanently lock the position cap.

## Key Functions

### `process_signal(signal, price, executor) -> Result<OrderAck, RiskError>`
- Runs all preflight checks
- Checks side-aware cooldown
- Checks position cap
- Submits order
- Updates cumulative tracking on success

### `PreflightChecker::check_signal(signal, price, net_delta) -> Result<(), RiskError>`
- 6 sequential checks: kill switch, daily loss, TTL, correlation, max notional, slippage
- Correlation check skipped for `impulse_obi` strategy

## Memory Layout (v0.1.3)

```
NetDelta:
┌──────────────────────────────────────────────────────────┐
│ positions: [[Option<Position>; 16]; 2]                   │
│ symbol_indices: Vec<(Symbol, usize)>                     │
│ daily_realized_pnl: f64                                  │
│ daily_loss_limit: f64                                    │
│ kill_switches: [Option<Arc<AtomicBool>>; 2]              │
└──────────────────────────────────────────────────────────┘

OrderManagementSystem:
┌──────────────────────────────────────────────────────────┐
│ risk_settings: RiskSettings                              │
│ strategy_settings: StrategySettings                      │
│ net_delta: NetDelta                                      │
│ preflight: PreflightChecker                              │
│ pending_orders: HashMap<String, OrderRequest>            │
│ last_trade_per_symbol: HashMap<(String, Side), u64>      │
└──────────────────────────────────────────────────────────┘
Note: `cumulative_notional`, `cumulative_size`, and `position_open_ts` were removed in v0.2.0 to eliminate state desync bugs.
```

## RiskError Variants

```rust
pub enum RiskError {
    ExceedsMaxNotional { notional, max },
    DailyDrawdownLimit { drawdown, max },
    ExcessiveSlippage { slippage_bps, max_bps },
    SignalExpired { age_ms, ttl_ms },
    SelfTrade,
    KillSwitchActive { venue },
    CorrelationTooLow { r, min },
    ExecutionFailed(String),  // Wraps any execution error
}
```
