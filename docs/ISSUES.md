# Engineering Issues Log

## Hot Path

| # | Issue | Latency Impact | Resolution Status |
|---|-------|----------------|-------------------|
| H1 | Ring buffer not using bitwise mask for power-of-2 sizes | +50ns per push | ✅ Resolved — using `(head + 1) & mask` |
| H2 | Correlation uses `sqrt()` which is slow on ARM | +200ns per calc | ⚠️ Deferred — could use fast approximation |
| H3 | No SIMD vectorization for lag search loop | +500ns for 21 lags | ⚠️ Deferred — requires `packed_simd` crate |
| H4 | `f64::is_finite()` check adds branch | +10ns per calc | ✅ Resolved — acceptable overhead for safety |
| H5 | Time-grid alignment allocates Vec per tick | +1µs per tick | ✅ Resolved — pre-allocated with capacity |

## OMS

| # | Issue | Latency Impact | Resolution Status |
|---|-------|----------------|-------------------|
| O1 | HashMap lookup for net_delta uses heap allocation | +200ns per lookup | ⚠️ Deferred — could use array index |
| O2 | Preflight checks run sequentially | +500ns total | ⚠️ Deferred — could parallelize |
| O3 | Self-trade prevention iterates all pending orders | O(n) per check | ✅ Resolved — acceptable for small n |
| O4 | Kill switch uses SeqCst ordering (overkill) | +50ns per check | ⚠️ Deferred — Relaxed would suffice |

## Infra

| # | Issue | Latency Impact | Resolution Status |
|---|-------|----------------|-------------------|
| I1 | No CPU pinning on macOS (only Linux) | Variable jitter | ✅ Resolved — conditional compilation |
| I2 | Telemetry writer uses BufWriter (not mmap) | +100µs per flush | ⚠️ Deferred — mmap adds complexity |
| I3 | Sled DB flush blocks on drop | +10ms shutdown | ✅ Resolved — background flush |
| I4 | No graceful WebSocket reconnection | N/A | ⚠️ Deferred — requires state recovery |
| I5 | Starlink latency spikes not detected | N/A | ⚠️ Deferred — needs RTT monitoring |

## Known Technical Debt

1. **Unused imports** — 43 compiler warnings for unused code. Low priority.
2. **No integration tests** — Only unit tests exist. Should add E2E tests.
3. **Mock exchange too simple** — Doesn't simulate partial fills or rejections.
4. **No metrics export** — Prometheus/Grafana integration missing.