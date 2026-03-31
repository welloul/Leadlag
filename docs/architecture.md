
## System Overview (v0.2.0 — MAKER ONLY)

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           TokioParasite v0.2.0                                  │
│                     Lead-Lag Arbitrage Engine (MAKER ONLY)                      │
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
│  │  ┌───────────────── ENTRY LOGIC (v0.2.0 MAKER) ─────────────────────────┐  │   │
│  │  │                                                                    │  │   │
│  │  │  Impulse + OBI Convergence ──▶ CALC MID PRICE ──▶ POST-ONLY LIMIT  │  │   │
│  │  │                                                                    │  │   │
│  │  │  FILL EVENT ──▶ AUTOMATED TAKE-PROFIT (+13.0 bps)                  │  │   │
│  │  │                                                                    │  │   │
│  │  │  ALPHA DECAY TIMEOUT ──▶ MARKET EXIT (IOC)                         │  │   │
│  │  └────────────────────────────────────────────────────────────────────┘  │   │
│  │                                                                          │   │
│  │  OMS GATES (v0.2.0):                                                     │   │
│  │  ├─ Cooldown: 200ms per (symbol, side)                                   │   │
│  │  ├─ Position cap: $100 per (venue, symbol)                               │   │
│  │  ├─ Maker check: Mid-price calculation + Post-Only tag                   │   │
│  │  ├─ Take-Profit: Auto-submit at +13 bps upon entry fill                  │   │
│  │  ├─ Tiered Exit: symbol_timeouts[sym] (1000ms - 2500ms)                  │   │
│  │  └─ Hot-Reload: 15s config watcher sync                                  │   │
│  │                                                                          │   │
│  │  signal → oms.process_signal() → PaperSimulator                          │   │
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

## Data Flow: Passive Lead-Lag Architecture (v0.2.0)

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                         MAKER STRATEGY DATA FLOW                                │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  IMPULSE-OBI PATH (v0.2.0):                                                     │
│  ──────────────────────────                                                     │
│  Tick A ──▶ process_tick() ──▶ Alpha Decay Probes (v0.2.0)                      │
│                                    │                                            │
│                                    ├─ Measure wall-clock between A move and B   │
│                                    ├─ Measure lead-lag edge window (latency)    │
│                                    └─ Output: Per-symbol convergence metric     │
│                                                                                 │
│  Tick B ──▶ ImpulseDetector.process_tick()                                      │
│                                    │                                            │
│                                    ├─ Lag: other |delta| < 1.0 bps?             │
│                                    ├─ Edge: cross-venue spread ≥ 4.5 bps?       │
│                                    └─ If all pass → ImpulseSignal               │
│                                                                                 │
│  Book A/B ──▶ process_book() ──▶ ObiDivergenceDetector                          │
│                                    │                                            │
│                                    ├─ Depth-weighted OBI (1/(i+1) weights)      │
│                                    └─ If yes → ObiSignal                        │
│                                                                                 │
│  ImpulseObiEngine combines:                                                     │
│  ├─ Pending impulse + incoming OBI → HIGH conviction                            │
│  └─ Timeout (250ms) clears pending signals                                      │
│                                                                                 │
│  OMS EXECUTION (THE MAKER SHIFT):                                               │
│  ├─ ENTRY (Post-Only): Place limit at mid-price.                                │
│  ├─ TP (Limit): Fill trigger -> Auto-Submit TP at entry + 13bps.                │
│  ├─ SL (Time-based): Loop checks age vs symbol_timeouts.                        │
│  └─ RELOAD: 15s heart-beat filesystem configuration refresh.                    │
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
│  │                                                                         │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## Memory Layout: Hot Path

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                         HOT PATH MEMORY LAYOUT                                  │
│                 (Zero-Allocation Runtime Engineering)                           │
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
│  IMPULSE-OBI PATH (MAKER):                                                      │
│  ─────────────────────────                                                      │
│  Component                    │ Cycles @3GHz │ Time      │ % of Budget         │
│  ─────────────────────────────┼──────────────┼───────────┼──────────────────── │
│  1. Tick ingestion            │ ~50          │ ~17ns     │ 0.2%                │
│  2. Alpha Probe (telemetry)   │ ~100         │ ~33ns     │ 0.3%                │
│  3. MidpriceTracker update    │ ~30          │ ~10ns     │ 0.1%                │
│  4. Threshold comparison      │ ~10          │ ~3ns      │ 0.03%               │
│  5. OMS Limit Calculation     │ ~100         │ ~33ns     │ 0.3%                │
│  ─────────────────────────────┼──────────────┼───────────┼──────────────────── │
│  TOTAL (impulse only)         │ ~290         │ ~96ns     │ 1.0%                │
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
│  HEADROOM: 90% (9µs available for OMS, network, etc.)                          │
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
