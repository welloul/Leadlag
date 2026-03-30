# OMS Module — Order Management System

## Objective
Validate trade signals against risk limits, track cross-venue positions, and execute orders. Acts as the "gatekeeper" between signal generation and order submission.

## Latency Profile

| Operation | O(n) | Cycles | Notes |
|-----------|------|--------|-------|
| Preflight checks | O(1) | ~500 | 6 sequential checks |
| Net delta lookup | O(1) | ~50 | Fixed-size array (was HashMap) |
| Self-trade check | O(p) | ~100 | p = pending orders |
| Order creation | O(1) | ~50 | Stack allocation |
| **Total** | **O(p)** | **~700** | **~1.1µs @ 3GHz** |

## Invariants

1. **Non-bypassable checks**: All 6 preflight checks must pass before order submission
2. **Kill switch atomic**: Uses `AtomicBool` with `SeqCst` ordering
3. **No blocking**: OMS never blocks on DB writes (optimistic updates)
4. **Self-trade prevention**: Cannot submit opposite side order to same venue/symbol
5. **Error propagation**: Execution errors wrapped in `RiskError::ExecutionFailed`
6. **Side-aware cooldown**: `(symbol, side)` key, 200ms between trades. Allows reversals.
7. **Position cap**: $100 cumulative notional per `(venue, symbol)`. Direction-aware: can reduce but not add beyond cap.

## Memory Layout (v0.1.3)

```
NetDelta (fixed-size array, O(1) lookup):
┌──────────────────────────────────────────────────────────┐
│ positions: [[Option<Position>; 16]; 2]                   │
│ symbol_indices: Vec<(Symbol, usize)>                     │
│ daily_realized_pnl: f64                                  │
│ daily_loss_limit: f64                                    │
│ kill_switches: [Option<Arc<AtomicBool>>; 2]              │
└──────────────────────────────────────────────────────────┘

OrderManagementSystem:
┌──────────────────────────────────────────────────────┐
│ risk_settings: RiskSettings                          │
│ strategy_settings: StrategySettings                  │
│ net_delta: NetDelta                                  │
│ preflight: PreflightChecker                          │
│ pending_orders: HashMap<String, OrderRequest>        │
│ last_trade_per_symbol: HashMap<(String,Side), u64>   │ ← Side-aware cooldown
│ cumulative_notional: HashMap<(String,String), f64>   │ ← Position cap
│ cumulative_size: HashMap<(String,String), f64>       │ ← Direction tracking
└──────────────────────────────────────────────────────┘
```
└─────────────────────────────────────────┘
```

## Preflight Checks (Sequential)

```
1. check_kill_switch()      → Is venue kill switch active?
2. check_daily_loss_limit() → Would this breach daily limit?
3. check_signal_ttl()       → Is signal still fresh (<150ms)?
4. check_correlation()      → Is R above minimum threshold?
5. check_max_notional()     → Would order exceed max size?
6. check_max_slippage()     → Would slippage exceed limit? (size-impact model)
```

## Key Functions

### `process_signal(signal, price, executor) -> Result<OrderAck, RiskError>`
- **Input**: Trade signal, current price, execution backend
- **Output**: Order acknowledgment or risk error
- **Side effects**: Creates pending order, submits to executor
- **Complexity**: O(p) where p = pending orders

### `process_fill(fill)`
- **Input**: Fill event from exchange
- **Output**: None
- **Side effects**: Updates net delta, removes pending order
- **Complexity**: O(1)

### `NetDelta::update_position(fill)`
- **Input**: Fill event
- **Output**: None
- **Side effects**: Updates position size, entry price, daily PnL
- **Complexity**: O(1)

## Risk Error Types (Updated v0.1.1)

```rust
pub enum RiskError {
    ExceedsMaxNotional { notional, max },
    DailyDrawdownLimit { drawdown, max },
    ExcessiveSlippage { slippage_bps, max_bps },
    SignalExpired { age_ms, ttl_ms },
    SelfTrade,
    KillSwitchActive { venue },
    CorrelationTooLow { r, min },
    ExecutionFailed(String),  // v0.1.1: wraps ExecutionError
}
```

**v0.1.1 Fix:** `ExecutionFailed` was added because previously execution errors were discarded and replaced with a misleading `ExceedsMaxNotional { 0.0, 0.0 }`. Now the original error message is preserved.
