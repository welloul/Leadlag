# Config Module — Settings & Validation

## Objective
Load, validate, and provide configuration from `settings.toml`. Fail-fast on invalid settings before any network connections are established.

## Latency Profile

| Operation | O(n) | Cycles | Notes |
|-----------|------|--------|-------|
| File read | O(1) | ~10000 | One-time at startup |
| TOML parse | O(n) | ~5000 | n = file size |
| Validation | O(n) | ~1000 | n = number of fields |
| **Total** | **O(n)** | **~16000** | **Startup only, not hot path** |

## Invariants

1. **Fail-fast**: Invalid config causes immediate panic at startup
2. **deny_unknown_fields**: Typos in TOML are caught
3. **Power-of-2 check**: `window_size_ticks` must be `2^k`
4. **Environment expansion**: `${VAR}` syntax for secrets

## Memory Layout (Updated v0.1.1)

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

StrategySettings (expanded for Impulse-OBI):
┌─────────────────────────────────────────┐
│ active_strategy: String                 │ ← "correlation_hysteresis" or "impulse_obi"
│ symbols: Vec<String>                    │
│ window_size_ticks: usize  (must be 2^k) │
│ min_correlation_r: f64    (0.5 - 1.0)   │
│ hysteresis_buffer: f64    (0.01 - 0.5)  │
│ enable_obi: bool                        │
│ obi_weight: f64           (0.0 - 1.0)   │
│                                         │
│ # Impulse-OBI settings                  │
│ impulse_threshold_bps: u64  (1-100)     │
│ lag_threshold_bps: u64      (1-50)      │
│ impulse_window_ms: u64      (1-100)     │
│ signal_timeout_ms: u64      (1-1000)    │
│ min_trade_size_filter: f64  (0.0-1.0)   │
│ spread_filter_bps: u64      (1-1000)    │
│ obi_strong_threshold: f64   (0.5-1.0)   │
│ obi_neutral_threshold: f64  (0.0-0.5)   │
│ obi_depth: usize            (1-100)     │
│ obi_spike_threshold: f64    (0.01-1.0)  │
└─────────────────────────────────────────┘

RiskSettings:
┌─────────────────────────────────────────┐
│ max_notional_usd: f64     (1 - 1M)      │
│ max_drawdown_daily: f64   (1 - 100K)    │
│ max_slippage_bps: u64     (1 - 100)     │
│ signal_ttl_ms: u64        (5 - 5000)    │
│ self_trade_prevention: bool             │
└─────────────────────────────────────────┘

SimulationSettings:
┌─────────────────────────────────────────┐
│ enabled: bool                           │
│ use_real_data: bool                     │ ← v0.1.1: fetch real market data
│ latency_simulation_ms: u64              │
│ fee_tier_bps: f64                       │
│ match_l2_depth: usize                   │
└─────────────────────────────────────────┘
```

## Key Functions

### `Settings::load() -> Result<Self>`
- **Input**: None (reads from file + env)
- **Output**: Validated settings
- **Side effects**: None
- **Complexity**: O(n)

## Validation Rules

| Field | Rule | Rationale |
|-------|------|-----------|
| `window_size_ticks` | Must be power of 2 | Bitwise mask optimization |
| `min_correlation_r` | 0.5 - 1.0 | Lower values generate noise |
| `hysteresis_buffer` | 0.01 - 0.5 | Prevents rapid flipping |
| `signal_ttl_ms` | 5 - 5000 | Too short = missed fills, too long = stale |
| `max_notional_usd` | 1 - 1,000,000 | Risk limit |
| `max_slippage_bps` | 1 - 100 | Execution quality |
| `impulse_threshold_bps` | 1 - 100 | Impulse detection sensitivity |
| `lag_threshold_bps` | 1 - 50 | Laggard flatness threshold |
| `impulse_window_ms` | 1 - 100 | Lookback window for price delta |

## settings.toml Structure (Updated v0.1.1)

```toml
[app]
log_level = "info"
perf_mode = true
cpu_pinning = true
tick_precision_ns = 5000000  # 5ms grid

[storage]
telemetry_path = "./data/telemetry/"
state_db_path = "./data/state_db"

[venues.exchange_a]
name = "binance_futures"
api_key = "${BINANCE_API_KEY}"
api_secret = "${BINANCE_API_SECRET}"
ws_url = "wss://fstream.binance.com/ws"
rest_url = "https://fapi.binance.com"
max_rtt_ms = 50

[venues.exchange_b]
name = "hyperliquid"
api_key = "${HL_API_KEY}"
api_secret = "${HL_API_SECRET}"
ws_url = "wss://api.hyperliquid.xyz/ws"
rest_url = "https://api.hyperliquid.xyz"
max_rtt_ms = 100

[strategy]
active_strategy = "impulse_obi"  # or "correlation_hysteresis"
symbols = ["ZEC", "XMR", "LINK"]
window_size_ticks = 256
min_correlation_r = 0.85
hysteresis_buffer = 0.10
enable_obi = true
obi_weight = 0.3

# Impulse-OBI settings
impulse_threshold_bps = 5
lag_threshold_bps = 1.5
impulse_window_ms = 5
signal_timeout_ms = 10
min_trade_size_filter = 0.001
spread_filter_bps = 10
obi_strong_threshold = 0.7
obi_neutral_threshold = 0.2
obi_depth = 5
obi_spike_threshold = 0.3

[risk]
max_notional_usd = 5000.0
max_drawdown_daily = 200.0
max_slippage_bps = 8
signal_ttl_ms = 150
self_trade_prevention = true

[simulation]
enabled = true
use_real_data = true        # Fetch real market data (no API keys needed)
latency_simulation_ms = 5
fee_tier_bps = 2.5
match_l2_depth = 10
```
