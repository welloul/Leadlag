# OMS Module — Order Management System (v0.1.3)

## Objective
Validate trade signals against risk limits, track positions, and execute orders. Implements position cap, side-aware cooldown, and conservative fill model.
Validate trade signals against risk limits, manage a passive market-making lifecycle, and automate exits. Implements `Post-Only` entries, automated Take-Profit limit orders, and tiered symbol-specific time exits.

## Invariants

1. **Non-bypassable checks**: All 6 preflight checks must pass
2. **Kill switch atomic**: `AtomicBool` with `SeqCst` ordering
3. **No blocking**: OMS never blocks on DB writes
4. **Self-trade prevention**: Cannot submit opposite side to same venue/symbol
5. **Error propagation**: Execution errors wrapped in `RiskError::ExecutionFailed`
6. **Side-aware cooldown**: `(symbol, side)` key, 200ms between trades. Allows reversals.
7. **Position cap**: $100 cumulative notional per `(venue, symbol)`. Direction-aware: can reduce but not add beyond cap.
8. **Maker Priority**: Entries MUST be `Post-Only`. Rejects if matching engine would cross the spread.
9. **TP Coupling**: Every entry fill MUST trigger a secondary Take-Profit limit order at +13 bps.

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
                ├─ [1] Resolve 'pending_orders'
                ├─ [2] Update net position & 'position_open_ts'
                └─ [3] GENERATE AUTOMATED TP:
                       Submit Limit Order at entry + 13.0 bps

Tick/Book ────▶ check_time_exits()
                │
                ├─ [1] Lookup symbol_timeouts[symbol] or default
                ├─ [2] Calculate position age (now - position_open_ts)
                └─ [3] IF age > timeout: Submit IOC Market-Exit
```

## Position Cap Logic

```rust
// At $100 cap:
if current_notional >= 100.0 {
    // Only allow if this trade REDUCES the position
    let would_reduce = (current_size > 0.0 && side == Sell)
        || (current_size < 0.0 && side == Buy);
    if !would_reduce {
        return Err("Position cap: ...");
    }
}
```

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
│ last_trade_per_symbol: HashMap<(String, Side), u64>      │ ← Side-aware cooldown
│ cumulative_notional: HashMap<(String, String), f64>      │ ← Position cap
│ cumulative_size: HashMap<(String, String), f64>          │ ← Direction tracking
└──────────────────────────────────────────────────────────┘
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
