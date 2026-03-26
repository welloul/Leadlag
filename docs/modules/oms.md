# OMS Module — Order Management System

## Objective
Validate trade signals against risk limits, track cross-venue positions, and execute orders. Acts as the "gatekeeper" between signal generation and order submission.

## Latency Profile

| Operation | O(n) | Cycles | Notes |
|-----------|------|--------|-------|
| Preflight checks | O(1) | ~500 | 6 sequential checks |
| Net delta lookup | O(n) | ~200 | HashMap iteration |
| Self-trade check | O(p) | ~100 | p = pending orders |
| Order creation | O(1) | ~50 | Stack allocation |
| **Total** | **O(n+p)** | **~850** | **~1.3µs @ 3GHz** |

## Invariants

1. **Non-bypassable checks**: All 6 preflight checks must pass before order submission
2. **Kill switch atomic**: Uses `AtomicBool` with `SeqCst` ordering
3. **No blocking**: OMS never blocks on DB writes (optimistic updates)
4. **Self-trade prevention**: Cannot submit opposite side order to same venue/symbol

## Memory Layout

```
NetDelta:
┌─────────────────────────────────────────┐
│ positions: HashMap<(VenueId,Symbol),Position> │ ← Heap allocated
│ daily_realized_pnl: f64                 │
│ daily_loss_limit: f64                   │
│ kill_switches: HashMap<VenueId,Arc<AtomicBool>>│
└─────────────────────────────────────────┘

OrderManagementSystem:
┌─────────────────────────────────────────┐
│ risk_settings: RiskSettings             │
│ strategy_settings: StrategySettings     │
│ net_delta: NetDelta                     │
│ preflight: PreflightChecker             │
│ pending_orders: HashMap<String,OrderRequest>│
└─────────────────────────────────────────┘
```

## Preflight Checks (Sequential)

```
1. check_kill_switch()      → Is venue kill switch active?
2. check_daily_loss_limit() → Would this breach daily limit?
3. check_signal_ttl()       → Is signal still fresh (<150ms)?
4. check_correlation()      → Is R above minimum threshold?
5. check_max_notional()     → Would order exceed max size?
6. check_max_slippage()     → Would slippage exceed limit?
```

## Key Functions

### `process_signal(signal, price, executor) -> Result<OrderAck, RiskError>`
- **Input**: Trade signal, current price, execution backend
- **Output**: Order acknowledgment or risk error
- **Side effects**: Creates pending order, submits to executor
- **Complexity**: O(n + p)

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

## Risk Error Types

```rust
pub enum RiskError {
    ExceedsMaxNotional { notional, max },
    DailyDrawdownLimit { drawdown, max },
    ExcessiveSlippage { slippage_bps, max_bps },
    SignalExpired { age_ms, ttl_ms },
    SelfTrade,
    KillSwitchActive { venue },
    CorrelationTooLow { r, min },
}