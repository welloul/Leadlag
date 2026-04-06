# Config Module — Settings & Validation (v0.6.1)

## Objective
Load, validate, and provide configuration from `settings.toml`. Supports **Hot-Reloading** of strategy parameters during runtime via a 15-second filesystem watcher. Fail-fast on invalid settings before any network connections are established.

## Invariants

1. **Fail-fast**: Invalid config causes immediate panic at startup
2. **deny_unknown_fields**: Typos in TOML are caught
3. **Power-of-2 check**: `window_size_ticks` must be `2^k`
4. **Environment expansion**: `${VAR}` syntax for secrets
5. **Hot-Reload safety**: Static validation must pass before swapping settings in memory

## Key Types (v0.6.0+)

```
TradingMode: "paper" | "live"
TargetExchange: "hyperliquid" | "okx" | "mexc"
```

These replace the old `simulation.enabled` boolean. Runner selection is determined by the combination of both fields.

## Memory Layout

```
Settings:
┌─────────────────────────────────────────┐
│ app: AppSettings                        │
│   trading_mode: TradingMode             │   ← NEW v0.6
│   target_exchange: TargetExchange       │   ← NEW v0.6
│   log_level: String                     │
│   perf_mode: bool                       │
│   cpu_pinning: bool                     │
│   tick_precision_ns: u64                │
│ storage: StorageSettings                │
│ venues: VenuesSettings                  │
│ strategy: StrategySettings              │
│ risk: RiskSettings                      │
│ simulation: SimulationSettings          │
└─────────────────────────────────────────┘

StrategySettings:
┌──────────────────────────────────────────────┐
│ active_strategy: String   ("impulse_obi")    │
│ symbols: Vec<String>      (bare base symbols)│
│ window_size_ticks: usize  (2^k)              │
│ min_correlation_r: f64    (0.5-1.0)          │
│ hysteresis_buffer: f64    (0.01-0.5)         │
│ enable_obi: bool                              │
│ obi_weight: f64           (0.0-1.0)          │
│ impulse_threshold_bps: f64 (1-500)           │
│ lag_threshold_bps: f64    (0.1-50)           │
│ impulse_window_ms: u64    (1-100)            │
│ signal_timeout_ms: u64    (1-1000)           │
│ min_trade_size_filter: f64                   │
│ spread_filter_bps: f64                       │
│ obi_strong_threshold: f64 (0.3-1.0)         │
│ obi_neutral_threshold: f64 (0.0-0.5)        │
│ obi_depth: usize          (1-20)            │
│ obi_spike_threshold: f64  (0.01-1.0)        │
│ venue_freshness_ms: u64   (50-2000)          │
│ entry_threshold_bps: f64  (0.1-50)           │
│ cooldown_ms: u64          (1-5000)           │
│ max_levels_consumed: usize (1-10)            │
│ obi_persist_ms: u64       (1-2000)           │
│ fill_conservatism: f64    (0.1-1.0)          │
│ high_conviction_only: bool                   │
│ exit_timeout_ms: u64                         │
│ take_profit_bps: f64                         │
│ symbol_timeouts: HashMap<String, u64>        │
└──────────────────────────────────────────────┘

RiskSettings:
┌─────────────────────────────────────────┐
│ max_notional_usd: f64     (1-1M)        │
│ max_position_usd: f64     (1-1M)        │
│ leverage: f64             (1-20)        │
│ max_drawdown_daily: f64   (1-100K)      │
│ max_slippage_bps: f64     (1-100)       │
│ signal_ttl_ms: u64        (5-5000)      │
│ self_trade_prevention: bool             │
└─────────────────────────────────────────┘

SimulationSettings:
┌─────────────────────────────────────────┐
│ use_real_data: bool                     │
│ latency_simulation_ms: u64             │
│ fee_tier_bps: f64                       │
│ match_l2_depth: usize                   │
└─────────────────────────────────────────┘
```

## settings.toml (v0.6.1 current)

```toml
[app]
trading_mode = "paper"          # "paper" | "live"
target_exchange = "okx"         # "hyperliquid" | "okx" | "mexc"
log_level = "info"
perf_mode = true
cpu_pinning = true
tick_precision_ns = 5000000     # 5ms grid

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
name = "hyperliquid"            # label only — actual venue set by target_exchange
api_key = "${HL_API_KEY}"
api_secret = "${HL_API_SECRET}"
ws_url = "wss://api.hyperliquid.xyz/ws"
rest_url = "https://api.hyperliquid.xyz"
max_rtt_ms = 100

[strategy]
active_strategy = "impulse_obi"
symbols = ["ZEC", "DOGE", "SUI", "SOL", "PUMP", "ADA", "WIF", "LINK", "XMR"]

# Impulse-OBI (production thresholds)
impulse_threshold_bps = 50.0
lag_threshold_bps = 1.0
impulse_window_ms = 5
signal_timeout_ms = 250
min_trade_size_filter = 0.001
spread_filter_bps = 0.0

obi_strong_threshold = 0.40
obi_neutral_threshold = 0.15
obi_depth = 5
obi_spike_threshold = 0.20
venue_freshness_ms = 400
entry_threshold_bps = 35.0
cooldown_ms = 15
max_levels_consumed = 5
obi_persist_ms = 30
fill_conservatism = 0.5
high_conviction_only = true
exit_timeout_ms = 2200
take_profit_bps = 8.0

[strategy.symbol_timeouts]
default = 2200
ADA = 1800
SOL = 1800
ZEC = 2000
LINK = 2200
XMR = 3000
SUI = 3000
PUMP = 2500
WIF = 2500

[risk]
max_notional_usd = 22.0
max_position_usd = 50.0
leverage = 5.0
max_drawdown_daily = 200.0
max_slippage_bps = 12.0
signal_ttl_ms = 500
self_trade_prevention = true

[simulation]
use_real_data = true
latency_simulation_ms = 5
fee_tier_bps = 2.5
match_l2_depth = 10
```

## Symbol Format Note

`symbols` in `settings.toml` must always be **bare base symbols** (e.g. `LINK`, not `LINKUSDT` or `LINK-USDT-SWAP`). Each exchange implementation converts to the appropriate venue format internally:

| Exchange | Converted to |
|----------|-------------|
| Binance trade WS | `{base}usdt` lowercase |
| Binance book WS | `{BASE}USDT` uppercase |
| OKX WS | `{BASE}-USDT-SWAP` |
| MEXC WS | `{BASE}_USDT` |
| Hyperliquid WS | `{BASE}` (no change) |
