# Config Module — Settings & Validation (v0.1.3)

## Objective
Load, validate, and provide configuration from `settings.toml`. Fail-fast on invalid settings before any network connections are established.

## Invariants

1. **Fail-fast**: Invalid config causes immediate panic at startup
2. **deny_unknown_fields**: Typos in TOML are caught
3. **Power-of-2 check**: `window_size_ticks` must be `2^k`
4. **Environment expansion**: `${VAR}` syntax for secrets

## Memory Layout (v0.1.3)

```
Settings:
┌─────────────────────────────────────────┐
│ app: AppSettings                        │
│ storage: StorageSettings                │
│ venues: VenuesSettings                  │
│ strategy: StrategySettings              │
│ risk: RiskSettings                      │
│ simulation: SimulationSettings          │
└─────────────────────────────────────────┘

StrategySettings (v0.1.3 — full list):
┌──────────────────────────────────────────────┐
│ active_strategy: String                      │
│ symbols: Vec<String>                         │
│ window_size_ticks: usize      (2^k)          │
│ min_correlation_r: f64        (0.5-1.0)      │
│ hysteresis_buffer: f64        (0.01-0.5)     │
│ enable_obi: bool                              │
│ obi_weight: f64               (0.0-1.0)      │
│ impulse_threshold_bps: u64    (1-100)         │
│ lag_threshold_bps: u64        (1-50)          │
│ impulse_window_ms: u64        (1-100)         │
│ signal_timeout_ms: u64        (1-1000)        │
│ min_trade_size_filter: f64    (0.0-1.0)       │
│ spread_filter_bps: u64        (1-1000)        │
│ obi_strong_threshold: f64     (0.5-1.0)       │
│ obi_neutral_threshold: f64    (0.0-0.5)       │
│ obi_depth: usize              (1-100)         │
│ obi_spike_threshold: f64      (0.01-1.0)      │
│                                             │
│ # v0.1.3 entry logic tightening             │
│ venue_freshness_ms: u64       (50-2000)      │
│ entry_threshold_bps: u64      (2-50)          │
│ cooldown_ms: u64              (50-5000)       │
│ max_levels_consumed: usize    (1-10)          │
│ obi_persist_ms: u64           (50-2000)       │
│ fill_conservatism: f64        (0.1-1.0)       │
└──────────────────────────────────────────────┘

RiskSettings:
┌─────────────────────────────────────────┐
│ max_notional_usd: f64     (1-1M)       │
│ max_drawdown_daily: f64   (1-100K)     │
│ max_slippage_bps: u64     (1-100)      │
│ signal_ttl_ms: u64        (5-5000)     │
│ self_trade_prevention: bool            │
└─────────────────────────────────────────┘

SimulationSettings:
┌─────────────────────────────────────────┐
│ enabled: bool                          │
│ use_real_data: bool                    │
│ latency_simulation_ms: u64             │
│ fee_tier_bps: f64                      │
│ match_l2_depth: usize                  │
└─────────────────────────────────────────┘
```

## Validation Rules (v0.1.3)

| Field | Rule | Rationale |
|-------|------|-----------|
| `window_size_ticks` | Must be power of 2 | Bitwise mask optimization |
| `signal_ttl_ms` | 5 - 5000 | Too short = missed fills, too long = stale |
| `max_notional_usd` | 1 - 1,000,000 | Per-trade risk limit |
| `venue_freshness_ms` | 50 - 2000 | Both venues must tick within this window |
| `entry_threshold_bps` | 2 - 50 | Must cover fees + slippage |
| `cooldown_ms` | 50 - 5000 | Side-aware cooldown |
| `obi_persist_ms` | 50 - 2000 | Time-based OBI persistence |
| `fill_conservatism` | 0.1 - 1.0 | Fraction of best level to fill |

## settings.toml (v0.1.3)

```toml
[app]
log_level = "info"
perf_mode = true
cpu_pinning = true
tick_precision_ns = 5000000

[storage]
telemetry_path = "./data/telemetry/"
state_db_path = "./data/state_db"

[venues.exchange_a]
name = "binance_futures"
ws_url = "wss://fstream.binance.com/ws"
rest_url = "https://fapi.binance.com"

[venues.exchange_b]
name = "hyperliquid"
ws_url = "wss://api.hyperliquid.xyz/ws"
rest_url = "https://api.hyperliquid.xyz"

[strategy]
active_strategy = "impulse_obi"
symbols = ["ZEC", "WLD", "FARTCOIN", "DOGE", "SUI", "BCH", "PUMP", "ADA"]
impulse_threshold_bps = 5
lag_threshold_bps = 1.5
impulse_window_ms = 5
signal_timeout_ms = 150
min_trade_size_filter = 0.001
spread_filter_bps = 10
obi_strong_threshold = 0.7
obi_neutral_threshold = 0.2
obi_depth = 5
obi_spike_threshold = 0.3
venue_freshness_ms = 400
entry_threshold_bps = 8
cooldown_ms = 200
max_levels_consumed = 3
obi_persist_ms = 200
fill_conservatism = 0.5

[risk]
max_notional_usd = 10.0
max_drawdown_daily = 200.0
max_slippage_bps = 8
signal_ttl_ms = 500
self_trade_prevention = true

[simulation]
enabled = true
use_real_data = true
latency_simulation_ms = 5
fee_tier_bps = 2.5
match_l2_depth = 10
```
