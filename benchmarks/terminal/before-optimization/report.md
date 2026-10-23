# Firecrab terminal benchmark

Recorded: 2026-10-23T03:58:43.751Z

Apple M5 / darwin arm64 / Chromium 151.0.7922.34

Packages: {"ghostty-web":"0.4.0","@xterm/xterm":"6.0.0","@xterm/addon-fit":"0.11.0"}

7 measured samples, 2 warmups, 4093-byte chunks, 5000-line scrollback.

Production build; local HTTP; installed system font fallback; one active page; alternating engine order. Output waits for the final marker in the parsed buffer and one following animation frame; write callbacks are not used for timing. Input is synthetic keydown with local echo and two frames; neither metric measures GPU presentation. Heap statistics are Chromium CDP observations, not total process memory. Fresh contexts do not imply a cold browser process or cold OS cache.

## Output to frame opportunity

| Grid | Workload | xterm median ms | Ghostty median ms | xterm MiB/s | Ghostty MiB/s | Correct samples |
|---|---|---:|---:|---:|---:|---|
| 80×24 | plain | 16.70 | 16.70 | 28.33 | 28.33 | 7/7; 7/7 |
| 80×24 | ansi | 24.90 | 16.60 | 21.75 | 32.63 | 7/7; 7/7 |
| 80×24 | unicode | 16.80 | 16.60 | 20.89 | 21.14 | 7/7; 7/7 |
| 80×24 | wrap | 25.00 | 16.60 | 32.96 | 49.64 | 7/7; 7/7 |
| 80×24 | tui | 16.70 | 16.60 | 27.98 | 28.15 | 7/7; 7/7 |
| 160×48 | plain | 25.10 | 16.70 | 18.85 | 28.33 | 7/7; 7/7 |
| 160×48 | ansi | 25.00 | 16.70 | 21.67 | 32.44 | 7/7; 7/7 |
| 160×48 | unicode | 16.80 | 16.60 | 20.89 | 21.14 | 7/7; 7/7 |
| 160×48 | wrap | 25.00 | 24.90 | 57.37 | 57.60 | 7/7; 7/7 |
| 160×48 | tui | 25.00 | 22.50 | 37.82 | 42.02 | 7/7; 7/7 |

## Startup, local echo and resize

| Engine | Fresh-context ready median ms | Input p50 ms | Input p95 ms | 40 resizes median ms |
|---|---:|---:|---:|---:|
| xterm | 19.10 | 16.60 | 16.80 | 16.50 |
| ghostty | 19.20 | 16.70 | 17.10 | 141.60 |

## Bundle payload

| Engine | Loaded resources bytes | Gzip bytes (computed, not transferred) |
|---|---:|---:|
| xterm | 343456 | 87389 |
| ghostty | 643584 | 188099 |

## Limits

- Loopback headless Chromium only. No guest VM, network latency, real OS IME, or GPU presentation timing is measured.
- A two-frame floor affects small workloads and synthetic input. Throughput includes scheduling and rendering opportunities, not just parsing.
- Memory captures JS heap and backing-storage observations before/after ten mount/dispose cycles, not a peak-memory or leak proof.
- Compare correctness before interpreting speed. Failed cases retain actual and expected screens in results.json.
- Timing is informational. No CI performance threshold is imposed from one machine.

Correctness: PASS
