# Telemetry Module — Persistence & Observability

## Objective
Persist raw ticks and lead-lag offset logs to binary files for replay. Write state to Sled embedded database for crash recovery. All writes are non-blocking to avoid stalling the hot path.

## Latency Profile

| Operation | O(n) | Cycles | Notes |
|-----------|------|--------|-------|
| Channel send | O(1) | ~50 | crossbeam try_send |
| Binary encode | O(1) | ~100 | prost or manual |
| BufWriter flush | O(b) | ~1000 | b = buffer size |
| **Total (hot path)** | **O(1)** | **~50** | **Only channel send** |

## Invariants

1. **Fire-and-forget**: Hot path never waits for disk writes
2. **Bounded channel**: `bounded(10000)` for backpressure
3. **try_send only**: Drop telemetry if channel full (log warning)
4. **Background thread**: Dedicated writer thread handles all I/O

## Memory Layout

```
TelemetryWriter:
┌─────────────────────────────────────────┐
│ sender: Sender<TelemetryEntry>          │
│ shutdown: Arc<AtomicBool>               │
│ handle: Option<JoinHandle<()>>          │
└─────────────────────────────────────────┘

TelemetryEntry (enum):
┌─────────────────────────────────────────┐
│ Tick(Tick)                              │
│ LeadLagOffset { timestamp, correlation, │
│                 lag_offset, lead_venue }│
│ Signal { timestamp, symbol, side, R }   │
└─────────────────────────────────────────┘

StateStore:
┌─────────────────────────────────────────┐
│ db: Arc<sled::Db>                       │
└─────────────────────────────────────────┘
```

## Key Functions

### `TelemetryWriter::new(base_path) -> Result<Self>`
- **Input**: Directory path for telemetry files
- **Output**: Writer with background thread
- **Side effects**: Spawns writer thread, creates directory
- **Complexity**: O(1)

### `TelemetryWriter::log_tick(tick)`
- **Input**: Tick to log
- **Output**: None (fire-and-forget)
- **Side effects**: Sends to channel (may drop if full)
- **Complexity**: O(1)

### `StateStore::store_position(venue, symbol, size, entry)`
- **Input**: Position data
- **Output**: Result
- **Side effects**: Writes to Sled DB
- **Complexity**: O(log n)

## Binary Format

```
Tick Entry:
[0x01][venue:1][price:8][size:8][ts:8] = 26 bytes

LeadLag Entry:
[0x02][ts:8][correlation:8][lag:8][lead:1] = 28 bytes

Signal Entry:
[0x03][ts:8][correlation:8][lag:8][sym_len:2][sym:N][side_len:2][side:M]
```

## File Rotation

- New file every 100,000 entries or on timestamp boundary
- Filename: `telemetry_{unix_timestamp}.bin`
- Old files kept for replay