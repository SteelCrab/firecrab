# Ghostty resize and bundle optimization

2026-10-23 · Apple M5 · macOS arm64 · headless Chromium 151.0.7922.34

Both runs use identical source fingerprints, package versions, fixtures and measurement code. Only the checked Vite optimizations toggle differs. Each run has two warmups and seven measured samples per condition.

| Metric | Unoptimized control | Optimized | Change |
|---|---:|---:|---:|
| 40 consecutive grid changes, ms | 140.10 | 16.60 | -88.2% |
| 40 paced frames, wall time ms | 349.30 | 349.30 | -0.0% |
| Paced resize synchronous handler p95, ms | 5.20 | 0.10 | -98.1% |
| Fresh-context startup median, ms | 19.60 | 19.50 | -0.5% |
| All harness resources, raw kB | 645.50 | 504.23 | -21.9% |
| All harness resources, computed gzip kB | 188.62 | 147.88 | -21.6% |
| Ghostty engine JavaScript only, raw kB | 636.23 | 71.91 | -88.7% |

## Cause and change

- The upstream resize path reset Canvas dimensions redundantly and rendered every intermediate grid synchronously. At device scale 2 it caused a third reset to restore the scaled backing store.
- The original instrumented 40-call sample spent about 1.5 ms in WASM resize and 129 ms in immediate rendering. Profiling timings are inclusive and are not statistical benchmark samples.
- The checked build patch preserves all 40 WASM resize calls and synchronous resize events. Canvas sizing and drawing use the existing animation loop once the final dirty state is available. No core resize is dropped or debounced.
- The package embedded 423,045 unchanged WASM bytes as 564,060 Base64 characters. Vite now emits the identical binary as a separate content-hashed asset. Total payload includes both JavaScript and WASM.
- The installed npm dependency stays untouched. Source-pattern and binary-equality checks fail explicitly if the locked package changes. The same plugin runs in dev, application builds and optimized benchmark builds.

## Interpretation

- Burst resize reaches the animation-frame floor and is close to the xterm control. This does not imply an equivalent increase in visible frame rate.
- Paced resize still takes about 349 ms for 40 frames in both runs. The synchronous handler is substantially shorter; drawing occurs in the scheduled frame.
- Startup and ordinary output remain broadly similar. Loopback timing does not establish a real-network loading gain.
- All 140 screen/cursor checks passed in each run. Browser regressions: 40 passed; one real-guest test skipped. Device-scale-2 Canvas dimensions, transform and visible text were checked.
- The original WASM memory-isolation and native complex-grapheme export accommodations remain in place.
- These are local Chromium results. Actual OS IME, guest tty resize and other browsers remain outside this measurement. Gzip values are computed artifact sizes, not captured compressed transfer sizes.

## Evidence

- [Original result before this optimization](before-optimization/report.md)
- [Same-harness unoptimized control](control/report.md)
- [Optimized run](latest/report.md)
- [Control raw samples](control/results.json)
- [Optimized raw samples](latest/results.json)
- [Original resize component profile](before-optimization/resize-profile.json)
- [Optimized resize component profile](latest/resize-profile.json)
