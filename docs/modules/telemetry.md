# Telemetry Module — Persistence & Observability

## Objective
Persist raw ticks and lead-lag offset logs to binary files for replay. Write state to Sled embedded database for crash recovery. All writes are non-blocking to avoid stalling the hot path.

## Invariants

1. **Fire-and-forget**: Hot path never waits for disk writes
2. **Bounded channel**: `bounded(10000)` for backpressure
3. **try_send only**: Drop telemetry if channel full (log warning)
4. **Background thread**: Dedicated writer thread handles all I/O

## Key Functions

### `TelemetryWriter::new(base_path) -> Result<Self>`
- Spawns background writer thread
- Creates output directory

### `TelemetryWriter::log_tick(tick)`
- Fire-and-forget: sends to channel, may drop if full

### `TelemetryWriter::log_signal(symbol, side, correlation, lag)`
- Logs signal generation for replay analysis

### `StateStore::open(path) -> Result<Self>`
- Opens Sled embedded database for crash recovery

### `StateStore::flush() -> Result<()>`
- Flushes pending writes

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
