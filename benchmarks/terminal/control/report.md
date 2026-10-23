# Firecrab terminal benchmark

Recorded: 2026-10-23T04:16:19.761Z

Apple M5 / darwin arm64 / Chromium 151.0.7922.34

Packages: {"ghostty-web":"0.4.0","@xterm/xterm":"6.0.0","@xterm/addon-fit":"0.11.0"}

7 measured samples, 2 warmups, 4093-byte chunks, 5000-line scrollback.

Production build; local HTTP; installed system font fallback; one active page; alternating engine order. Output waits for the final marker in the parsed buffer and one following animation frame; write callbacks are not used for timing. Input is synthetic keydown with local echo and two frames; neither metric measures GPU presentation. Heap statistics are Chromium CDP observations, not total process memory. Fresh contexts do not imply a cold browser process or cold OS cache.

## Output to frame opportunity

| Grid | Workload | xterm median ms | Ghostty median ms | xterm MiB/s | Ghostty MiB/s | Correct samples |
|---|---|---:|---:|---:|---:|---|
| 80×24 | plain | 24.90 | 16.70 | 19.00 | 28.33 | 7/7; 7/7 |
| 80×24 | ansi | 16.60 | 16.70 | 32.63 | 32.44 | 7/7; 7/7 |
| 80×24 | unicode | 16.70 | 16.70 | 21.02 | 21.02 | 7/7; 7/7 |
| 80×24 | wrap | 25.00 | 16.60 | 32.96 | 49.64 | 7/7; 7/7 |
| 80×24 | tui | 16.60 | 16.60 | 28.15 | 28.15 | 7/7; 7/7 |
| 160×48 | plain | 18.50 | 16.60 | 25.57 | 28.50 | 7/7; 7/7 |
| 160×48 | ansi | 24.90 | 16.70 | 21.75 | 32.44 | 7/7; 7/7 |
| 160×48 | unicode | 16.80 | 16.60 | 20.89 | 21.14 | 7/7; 7/7 |
| 160×48 | wrap | 25.00 | 24.10 | 57.37 | 59.52 | 7/7; 7/7 |
| 160×48 | tui | 25.00 | 21.80 | 37.82 | 43.37 | 7/7; 7/7 |

## Startup, local echo and resize

| Engine | Fresh-context ready median ms | Input p50 ms | Input p95 ms | 40 resizes median ms |
|---|---:|---:|---:|---:|
| xterm | 18.90 | 16.60 | 16.80 | 16.70 |
| ghostty | 19.60 | 16.70 | 18.00 | 140.10 |

## Bundle payload

| Engine | Loaded resources bytes | Gzip bytes (computed, not transferred) |
|---|---:|---:|
| xterm | 345377 | 87915 |
| ghostty | 645505 | 188624 |

## Paced resize (one grid change per animation frame)

| Engine | 40 frames median ms | Handler sum median ms | Handler p95 ms |
|---|---:|---:|---:|
| xterm | 349.10 | 4.60 | 0.20 |
| ghostty | 349.30 | 118.40 | 5.20 |

## Limits

- Loopback headless Chromium only. No guest VM, network latency, real OS IME, or GPU presentation timing is measured.
- A two-frame floor affects small workloads and synthetic input. Throughput includes scheduling and rendering opportunities, not just parsing.
- Memory captures JS heap and backing-storage observations before/after ten mount/dispose cycles, not a peak-memory or leak proof.
- Compare correctness before interpreting speed. Failed cases retain actual and expected screens in results.json.
- Timing is informational. No CI performance threshold is imposed from one machine.

Correctness: PASS
