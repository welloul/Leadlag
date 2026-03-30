# OMS Module — Order Management System (v0.1.3)

## Objective
Validate trade signals against risk limits, track positions, and execute orders. Implements position cap, side-aware cooldown, and conservative fill model.

## Invariants

1. **Non-bypassable checks**: All 6 preflight checks must pass
2. **Kill switch atomic**: `AtomicBool` with `SeqCst` ordering
3. **No blocking**: OMS never blocks on DB writes
4. **Self-trade prevention**: Cannot submit opposite side to same venue/symbol
5. **Error propagation**: Execution errors wrapped in `RiskError::ExecutionFailed`
6. **Side-aware cooldown**: `(symbol, side)` key, 200ms between trades. Allows reversals.
7. **Position cap**: $100 cumulative notional per `(venue, symbol)`. Direction-aware: can reduce but not add beyond cap.

## Order Flow (v0.1.3)

```
TradeSignal ──▶ process_signal()
                │
                ├─ [1] Side-aware cooldown check (200ms per symbol+side)
                ├─ [2] Preflight: kill switch, daily loss, signal TTL, correlation, max notional, slippage
                ├─ [3] Self-trade prevention
                ├─ [4] Position cap check ($100, direction-aware)
                ├─ [5] Calculate order size (max_notional / price)
                ├─ [6] Submit order to executor
                └─ [7] Update cooldown + cumulative notional on success
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
