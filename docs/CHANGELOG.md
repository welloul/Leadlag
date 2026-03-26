# Changelog

All notable changes to TokioParasite are documented here.

Format: `[Version] — Date — Category`

Categories: `GC Pressure`, `Allocation Changes`, `Math Optimizations`, `Architecture`, `Bug Fix`

---

## [0.1.0] — 2026-03-26 — Initial Release

### Architecture
- Complete modular architecture with EAL, Signal Pipeline, OMS, Simulator, Persistence
- Exchange Abstraction Layer with trait-based design (`MarketData`, `OrderExecution`)
- Paper trading simulator with L2 order book matching
- Sled embedded database for state persistence
- Proto3 binary telemetry writer

### Math Optimizations
- **Incremental Pearson correlation** with O(1) running sums
- **Power-of-2 ring buffer** with bitwise mask indexing (`& mask` instead of `%`)
- **Defensive math**: epsilon guards, NaN/Inf protection, clamping to [-1, 1]
- **Lag detection**: correlation at multiple time offsets for lead identification

### Allocation Changes
- **Zero allocations on hot path**: pre-allocated ring buffers, no Vec::push
- **Stack-only hot path types**: `HotPathError` is `#[repr(u8)]` enum
- **Arc-wrapped shared data**: `Arc<Tick>` for zero-copy fan-out
- **Bounded channels**: `crossbeam_channel::bounded` for backpressure

### GC Pressure
- **No GC**: Rust's ownership model eliminates GC pressure
- **No heap allocations in hot path**: all data on stack or pre-allocated
- **Atomic operations**: `AtomicBool` for kill switch (no locks)

### Bug Fix
- Fixed `update_position` not updating size on first fill (OMS module)
- Fixed correlation lag direction (was `i + lag`, now `i - lag`)
- Fixed hysteresis test using values that don't exceed threshold margin

---

## [Unreleased]

### Planned
- SIMD vectorization for lag search loop (`packed_simd` crate)
- Fast sqrt approximation for correlation denominator
- WebSocket reconnection with state recovery
- Prometheus metrics export
- Integration tests with real exchange sandbox APIs