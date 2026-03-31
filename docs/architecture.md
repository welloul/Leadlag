
## System Overview (v0.1.4)

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           TokioParasite v0.1.4                                  │
│                     Lead-Lag Arbitrage Engine                                    │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                        ASYNC I/O ZONE                                    │   │
│  │  ┌───────────────────┐    ┌───────────────────┐                         │   │
│  │  │ Binance WS        │    │ Hyperliquid WS    │                         │   │
│  │  │ @trade + @depth   │    │ trades + l2Book   │                         │   │
│  │  └─────┬──────┬──────┘    └─────┬──────┬──────┘                         │   │
│  │        │ Tick  │ Book           │ Tick  │ Book                          │   │
│  │        ▼       ▼                ▼       ▼                               │   │
│  │  crossbeam::bounded(1024)    crossbeam::bounded(1024)                  │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                    │           │           │           │                        │
│                    ▼           ▼           ▼           ▼                        │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                      MAIN LOOP (Async Tokio Task)                        │   │
│  │                                                                          │   │
│  │  ┌───────────────── ENTRY LOGIC (v0.1.4) ─────────────────────────────┐  │   │
│  │  │                                                                    │  │   │
│  │  │  Impulse path:                                                     │  │   │
│  │  │  ├─ Freshness gate (400ms local)                                   │  │   │
│  │  │  ├─ Warmup gate (both trackers init + warmed)                      │  │   │
│  │  │  ├─ Sanity check (delta < 500 bps)                                 │  │   │
│  │  │  ├─ Lag check (other delta < 1.0 bps)                              │  │   │
│  │  │  ├─ Momentum filter (current delta agrees with previous sign)     │  │   │
  │  │  ├─ Edge check (5 bps fees-aware, direction-normalized)            │  │   │
│  │  │  └─ Cooldown (200ms per symbol+side)                               │  │   │
│  │  │                                                                    │  │   │
│  │  │  OBI path:                                                         │  │   │
│  │  │  ├─ Weighted OBI (depth-weighted, top levels dominate)             │  │   │
│  │  │  ├─ Time-based persistence (30ms, not count-based)                 │  │   │
│  │  │  └─ Edge check (same fees-aware threshold)                         │  │   │
│  │  │                                                                    │  │   │
│  │  │  Position cap: $100 per (venue, symbol), direction-aware           │  │   │
│  │  │  Book age gate: 400ms hard reject                                  │  │   │
│  │  │  Conservative fill: 50% of best level size                        │  │   │
│  │  └────────────────────────────────────────────────────────────────────┘  │   │
│  │                                                                          │   │
│  │  signal → oms.process_signal() → PaperSimulator                         │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                     │                          │
│                                                     ▼                          │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                    PERSISTENCE ZONE (Background Threads)                │   │
│  │  ┌───────────────┐   ┌───────────────┐   ┌───────────────┐              │   │
│  │  │  Telemetry    │   │  State Store  │   │  Structured   │              │   │
│  │  │  Writer       │   │  (Sled DB)    │   │  Logging      │              │   │
│  │  │  (Proto3)     │   │               │   │  (tracing)    │              │   │
│  │  └───────────────┘   └───────────────┘   └───────────────┘              │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## Data Flow: Dual-Strategy Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                         DUAL-STRATEGY DATA FLOW                                 │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  CORRELATION-HYSTERESIS PATH:                                                   │
│  ─────────────────────────────                                                  │
│  Tick A ──┐                                                                     │
│           ├──▶ TimeGrid.ingest_tick() ──▶ AlignedPair ──▶ process_pair()       │
│  Tick B ──┘     (forward-fill)              (price_a,       │                  │
│                                              price_b)        │                  │
│                                                              ▼                  │
│                                                   CrossCorrelator.push()        │
│                                                              │                  │
│                                                              ▼                  │
│                                                   find_best_lag(-10, 10)        │
│                                                              │                  │
│                                                              ▼                  │
│                                                   Hysteresis.update(r_a, r_b)   │
│                                                              │                  │
│                                                   ┌──────────┴──────────┐       │
│                                                   │  Role flip?         │       │
│                                                   │  best_r >= 0.85?    │       │
│                                                   └──────────┬──────────┘       │
│                                                              │ Yes              │
│                                                              ▼                  │
│                                                        TradeSignal              │
│                                                                                 │
│  IMPULSE-OBI PATH (v0.1.4):                                                    │
│  ──────────────────────────                                                     │
│  Tick A ──▶ process_tick() ──▶ ImpulseDetector.process_tick()                   │
│                                    │                                            │
│                                    ├─ Route to correct tracker (venue-based)    │
│                                    ├─ Local freshness check (400ms)             │
│                                    ├─ Both trackers init + warmed up?           │
│                                    ├─ Sanity: delta < 500 bps?                  │
│                                    ├─ Lag: other |delta| < 1.0 bps?             │
│                                    ├─ Edge: cross-venue spread ≥ 5 bps?         │
│                                    └─ If all pass → ImpulseSignal               │
│                                                                                 │
│  Book A ──▶ process_book() ──▶ ObiDivergenceDetector.process_book()             │
│                                    │                                            │
│                                    ├─ Depth-weighted OBI (1/(i+1) weights)      │
│                                    ├─ Time-based persistence (30ms)            │
│                                    ├─ Divergence: one strong, other neutral?    │
│                                    └─ If yes → ObiSignal                        │
│                                                                                 │
│  ImpulseObiEngine combines:                                                     │
│  ├─ Edge check (5 bps direction-normalized)                                    │
│  ├─ Pending impulse + incoming OBI → HIGH conviction                           │
│  ├─ Pending OBI + incoming impulse → HIGH conviction                           │
│  ├─ Impulse only → MEDIUM conviction                                           │
│  ├─ OBI only → MEDIUM conviction                                               │
│  └─ Timeout (250ms) clears pending signals                                     │
│                                                                                 │
│  OMS gates:                                                                    │
│  ├─ Cooldown: 200ms per (symbol, side)                                         │
│  ├─ Position cap: $100 per (venue, symbol), direction-aware                    │
│  ├─ Book age gate: 400ms hard reject                                           │
│  ├─ Conservative fill: 50% of best level                                       │
│  └─ TTL: 500ms signal expiry                                                   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## Hysteresis State Machine

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                         HYSTERESIS STATE MACHINE                                │
│                    (Streak-Based, No Magnitude Check)                           │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│                              ┌─────────────────┐                                │
│                              │  UNDETERMINED   │                                │
│                              │  (Initial)      │                                │
│                              └────────┬────────┘                                │
│                                       │                                         │
│                                       │ First update(r_a, r_b)                  │
│                                       │ r_a > r_b → A leads                     │
│                                       │ r_b > r_a → B leads                     │
│                                       ▼                                         │
│                    ┌──────────────────────────────────────┐                     │
│                    │                                      │                     │
│           ┌────────┴────────┐                    ┌────────┴────────┐            │
│           │    A LEADS      │                    │    B LEADS      │            │
│           │    current_r=r_a│                    │    current_r=r_b│            │
│           └────────┬────────┘                    └────────┬────────┘            │
│                    │                                      │                     │
│                    │ B dominant (any margin)              │ A dominant          │
│                    │ r_b > r_a                            │ r_a > r_b           │
│                    │                                      │                     │
│                    ▼                                      ▼                     │
│           ┌─────────────────┐                    ┌─────────────────┐            │
│           │ B CANDIDATE     │                    │ A CANDIDATE     │            │
│           │ streak = 1      │                    │ streak = 1      │            │
│           └────────┬────────┘                    └────────┬────────┘            │
│                    │                                      │                     │
│                    │ B still dominant                     │ A still dominant    │
│                    │ streak++                             │ streak++            │
│                    ▼                                      ▼                     │
│           ┌─────────────────┐                    ┌─────────────────┐            │
│           │ B CANDIDATE     │                    │ A CANDIDATE     │            │
│           │ streak = 2      │                    │ streak = 2      │            │
│           └────────┬────────┘                    └────────┬────────┘            │
│                    │                                      │                     │
│                    │ streak >= min_consecutive            │ streak >= min       │
│                    │ (e.g., 3)                            │                     │
│                    ▼                                      ▼                     │
│           ┌─────────────────┐                    ┌─────────────────┐            │
│           │    B LEADS      │                    │    A LEADS      │            │
│           │    (FLIP!)      │                    │    (FLIP!)      │            │
│           └─────────────────┘                    └─────────────────┘            │
│                                                                                 │
│  KEY: Flip based on consistent leader change (streak) only.                     │
│  Correlation quality filtered by min_correlation_r.                             │
│                                                                                 │
│  STREAK RESET CONDITIONS:                                                       │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ If current lead reasserts dominance:                                    │   │
│  │   - candidate_streak = 0                                                │   │
│  │   - candidate_lead = Undetermined                                       │   │
│  │                                                                         │   │
│  │ If new candidate appears (different from previous candidate):           │   │
│  │   - candidate_streak = 1 (reset to 1, not 0)                           │   │
│  │   - candidate_lead = new candidate                                      │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## Memory Layout: Hot Path

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                         HOT PATH MEMORY LAYOUT                                  │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  RingBuffer<256> (2088 bytes, fits in 32 cache lines)                          │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ Offset │ Field      │ Size    │ Purpose                                 │   │
│  ├────────┼────────────┼─────────┼─────────────────────────────────────────┤   │
│  │ 0x0000 │ data[0]    │ 8 bytes │ First price value                       │   │
│  │ 0x0008 │ data[1]    │ 8 bytes │ Second price value                      │   │
│  │  ...   │ ...        │ ...     │ ...                                     │   │
│  │ 0x07F8 │ data[255]  │ 8 bytes │ Last price value                        │   │
│  │ 0x0800 │ head       │ 8 bytes │ Write position (0-255)                  │   │
│  │ 0x0808 │ len        │ 8 bytes │ Valid element count                     │   │
│  │ 0x0810 │ mask       │ 8 bytes │ 255 (for bitwise AND)                   │   │
│  │ 0x0818 │ sum        │ 8 bytes │ Running sum Σx                          │   │
│  │ 0x0820 │ sum_sq     │ 8 bytes │ Running sum Σx²                         │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
│  CrossCorrelator<256> (4192 bytes, fits in 65 cache lines)                     │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ Offset │ Field      │ Size    │ Purpose                                 │   │
│  ├────────┼────────────┼─────────┼─────────────────────────────────────────┤   │
│  │ 0x0000 │ buf_a      │ 2088 B  │ Ring buffer for exchange A              │   │
│  │ 0x0828 │ buf_b      │ 2088 B  │ Ring buffer for exchange B              │   │
│  │ 0x1050 │ sum_ab     │ 8 bytes │ Running cross-sum Σ(a*b)                │   │
│  │ 0x1058 │ epsilon    │ 8 bytes │ 1e-12 (defensive division)              │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
│  Hysteresis (80 bytes, fits in 2 cache lines)                                  │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ Offset │ Field           │ Size    │ Purpose                            │   │
│  ├────────┼─────────────────┼─────────┼────────────────────────────────────┤   │
│  │ 0x0000 │ current_lead    │ 1 byte  │ LeadRole enum                      │   │
│  │ 0x0008 │ current_r       │ 8 bytes │ Current lead correlation           │   │
│  │ 0x0010 │ candidate_lead  │ 1 byte  │ Candidate LeadRole                 │   │
│  │ 0x0018 │ candidate_r     │ 8 bytes │ Candidate correlation              │   │
│  │ 0x0020 │ candidate_streak│ 4 bytes │ Consecutive dominance count        │   │
│  │ 0x0028 │ threshold_margin│ 8 bytes │ Stored but unused (streak-based)   │   │
│  │ 0x0030 │ min_consecutive │ 4 bytes │ Required streak length             │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
│  IngestResult (5200 bytes, stack-allocated)                                     │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ pairs: [AlignedPair; 64]   (5120 bytes)    │ Fixed-size array           │   │
│  │ count: usize               (8 bytes)        │ Valid pair count           │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
│  PendingSignal (17 bytes, Copy-friendly, no heap allocation)                    │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ venue: VenueId        (1 byte)     │ Target venue                       │   │
│  │ side: OrderSide       (1 byte)     │ Buy or Sell                        │   │
│  │ timestamp_ns: u64     (8 bytes)    │ Signal timestamp                   │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
│  TOTAL HOT PATH MEMORY: ~11.5KB (180 cache lines)                              │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## Channel Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           CHANNEL ARCHITECTURE                                  │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                    ASYNC PRODUCERS (Tokio Tasks)                         │   │
│  │                                                                         │   │
│  │  ┌───────────────┐       ┌───────────────┐                              │   │
│  │  │ WS Task A     │       │ WS Task B     │                              │   │
│  │  │ (Binance)     │       │ (Hyperliquid) │                              │   │
│  │  └───────┬───┬───┘       └───────┬───┬───┘                              │   │
│  │          │   │                   │   │                                  │   │
│  │          │   │ Arc<BookUpdate>   │   │ Arc<BookUpdate>                  │   │
│  │          │   │ try_send          │   │ try_send                         │   │
│  │          │   ▼                   │   ▼                                  │   │
│  │          │ Book Channel A        │ Book Channel B                       │   │
│  │          │                       │                                      │   │
│  │          │ Arc<Tick>             │ Arc<Tick>                            │   │
│  │          │ try_send              │ try_send                             │   │
│  │          ▼                       ▼                                      │   │
│  │  ┌─────────────────────────────────────────────────────────────────┐   │   │
│  │  │              crossbeam::bounded(1024)                           │   │   │
│  │  │              Tick A     Tick B     Book A     Book B             │   │   │
│  │  └─────────────────────────────────────────────────────────────────┘   │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                    │           │           │           │                        │
│                    │ try_recv  │ try_recv  │ try_recv  │ try_recv               │
│                    ▼           ▼           ▼           ▼                        │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                    MAIN LOOP (Async Tokio Task)                         │   │
│  │                                                                         │   │
│  │  Strategy routing:                                                      │   │
│  │  ├─ Correlation-Hysteresis: tick → timegrid → process_pair()           │   │
│  │  ├─ Impulse-OBI: tick → process_tick()                                │   │
│  │  └─ Impulse-OBI: book → process_book()                                │   │
│  │                                                                         │   │
│  │  signal → oms.process_signal() → PaperSimulator.submit_order()        │   │
│  │                                                                         │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
│  BACKPRESSURE STRATEGY:                                                         │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ • Hot path uses try_send() — never blocks                               │   │
│  │ • If channel full → drop tick, log warning                              │   │
│  │ • If signal channel full → drop signal (stale anyway)                   │   │
│  │ • Bounded channels prevent memory bloat                                 │   │
│  │ • Book subscriptions optional — graceful fallback if unavailable        │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## Component Dependencies

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                         COMPONENT DEPENDENCY GRAPH                              │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│                              ┌─────────────┐                                    │
│                              │   main.rs   │                                    │
│                              │ (Orchestrator)                                   │
│                              └──────┬──────┘                                    │
│                                     │                                           │
│                    ┌────────────────┼────────────────┐                          │
│                    │                │                │                          │
│                    ▼                ▼                ▼                          │
│             ┌─────────────┐  ┌─────────────┐  ┌─────────────┐                  │
│             │   config    │  │    eal      │  │   logging   │                  │
│             │  (Settings) │  │  (Traits)   │  │  (tracing)  │                  │
│             └─────────────┘  └──────┬──────┘  └─────────────┘                  │
│                                     │                                           │
│                    ┌────────────────┼────────────────┐                          │
│                    │                │                │                          │
│                    ▼                ▼                ▼                          │
│             ┌─────────────┐  ┌─────────────┐  ┌─────────────┐                  │
│             │   signal    │  │    oms      │  │    sim      │                  │
│             │ (Hot Path)  │  │  (Risk)     │  │ (Paper)     │                  │
│             └──────┬──────┘  └──────┬──────┘  └──────┬──────┘                  │
│                    │                │                │                          │
│                    │                ▼                │                          │
│                    │         ┌─────────────┐         │                          │
│                    │         │   persist   │         │                          │
│                    │         │ (Telemetry) │         │                          │
│                    │         └─────────────┘         │                          │
│                    │                                  │                          │
│  ─────────────────────────────────────────────────────────────────────────────  │
│                                                                                 │
│  DEPENDENCY RULES:                                                              │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ • config: No dependencies (loaded first)                                │   │
│  │ • eal: Depends on config only                                           │   │
│  │ • signal: Depends on eal::types only (no async)                         │   │
│  │ • oms: Depends on eal, config                                           │   │
│  │ • sim: Depends on eal, config                                           │   │
│  │ • persist: Depends on eal::types                                        │   │
│  │ • logging: No dependencies                                              │   │
│  │ • main: Depends on all modules                                          │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## Latency Budget

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                            LATENCY BUDGET (<10µs target)                        │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  CORRELATION-HYSTERESIS PATH:                                                   │
│  ─────────────────────────────                                                  │
│  Component                    │ Cycles @3GHz │ Time      │ % of Budget         │
│  ─────────────────────────────┼──────────────┼───────────┼──────────────────── │
│  1. Tick ingestion            │ ~50          │ ~17ns     │ 0.2%                │
│  2. Time-grid alignment       │ ~100         │ ~33ns     │ 0.3%                │
│  3. Ring buffer push          │ ~50          │ ~17ns     │ 0.2%                │
│  4. Running sum update        │ ~20          │ ~7ns      │ 0.1%                │
│  5. Pearson correlation       │ ~100         │ ~33ns     │ 0.3%                │
│  6. Lag search (21 lags)      │ ~2100        │ ~700ns    │ 7.0%                │
│  7. Hysteresis update         │ ~30          │ ~10ns     │ 0.1%                │
│  8. Signal generation         │ ~50          │ ~17ns     │ 0.2%                │
│  ─────────────────────────────┼──────────────┼───────────┼──────────────────── │
│  TOTAL                        │ ~2500        │ ~834ns    │ 8.3%                │
│                                                                                 │
│  IMPULSE-OBI PATH:                                                              │
│  ─────────────────                                                              │
│  Component                    │ Cycles @3GHz │ Time      │ % of Budget         │
│  ─────────────────────────────┼──────────────┼───────────┼──────────────────── │
│  1. Tick ingestion            │ ~50          │ ~17ns     │ 0.2%                │
│  2. MidpriceTracker update    │ ~30          │ ~10ns     │ 0.1%                │
│  3. Delta bps calculation     │ ~20          │ ~7ns      │ 0.1%                │
│  4. Threshold comparison      │ ~10          │ ~3ns      │ 0.03%               │
│  5. PendingSignal store       │ ~5           │ ~2ns      │ 0.02%               │
│  ─────────────────────────────┼──────────────┼───────────┼──────────────────── │
│  TOTAL (impulse only)         │ ~115         │ ~39ns     │ 0.4%                │
│                                                                                 │
│  Component                    │ Cycles @3GHz │ Time      │ % of Budget         │
│  ─────────────────────────────┼──────────────┼───────────┼──────────────────── │
│  1. Book ingestion            │ ~50          │ ~17ns     │ 0.2%                │
│  2. OBI calculation           │ ~100         │ ~33ns     │ 0.3%                │
│  3. Divergence check          │ ~30          │ ~10ns     │ 0.1%                │
│  4. PendingSignal store       │ ~5           │ ~2ns      │ 0.02%               │
│  ─────────────────────────────┼──────────────┼───────────┼──────────────────── │
│  TOTAL (OBI only)             │ ~185         │ ~62ns     │ 0.6%                │
│                                                                                 │
│  HEADROOM: 91.4% (9.1µs available for OMS, network, etc.)                      │
│                                                                                 │
│  ─────────────────────────────────────────────────────────────────────────────  │
│                                                                                 │
│  OPTIMIZATION OPPORTUNITIES:                                                    │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ • SIMD for lag search: ~2100ns → ~300ns (7x speedup)                   │   │
│  │ • Fast sqrt: ~33ns → ~10ns (3x speedup)                                │   │
│  │ • Pre-computed masks: ~17ns → ~5ns (3x speedup)                        │   │
│  │                                                                         │   │
│  │ With all optimizations: ~350ns total (3.5% of budget)                   │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```
