# WASM terminal prototype and benchmarks

The Firecrab console prototype uses `ghostty-web` **0.4.0** with its
Ghostty WebAssembly core and Canvas 2D renderer. This evaluates an existing
open-source integration before building a direct official `libghostty-vt` adapter.
The guest still runs on the existing shared serial-console connection.

See [the optimization comparison](../../benchmarks/terminal/comparison.md) for
the latest cause analysis and same-harness control/optimized measurements.

## Try the console

Run the dashboard and API as described in the development guide. Open a VM's
console normally to use Ghostty. To compare with the existing xterm engine,
put `?terminalEngine=xterm` before the hash route:

```text
http://localhost:8080/?terminalEngine=xterm#/console/<vm-id>
```

The query override is for diagnostics; it does not change the guest or create a
different shell. Display preferences, session end code 4000, reconnection,
terminal-only mode, SSH instructions, and log export keep their existing roles.

## Reproduce the benchmark

```sh
npm ci --prefix firecrab-frontend
npm ci --prefix firecrab-e2e
npm exec --prefix firecrab-e2e -- playwright install chromium
npm run benchmark:terminal --prefix firecrab-e2e
```

The runner builds an isolated harness with Vite's production optimizations,
serves it on a loopback-only ephemeral port, and closes its server and browser
afterward. It requires no API, VM, KVM, or external website.

Outputs are written to `benchmarks/terminal/latest/`:

- `report.md`: comparison tables and measurement limits.
- `results.json`: environment, package/source fingerprints, raw samples,
  correctness checks, and CDP heap observations.
- `output.csv`: per-sample output timings and throughput.
- `ghostty.png` and `xterm.png`: the Unicode workload as rendered.

The original run is preserved in `benchmarks/terminal/before-optimization/`.
For a same-harness control run with the upstream resize/inline-WASM behavior:

```sh
FIRECRAB_TERMINAL_BENCH_UNOPTIMIZED=1 \
  FIRECRAB_TERMINAL_BENCH_OUTPUT="$PWD/benchmarks/terminal/control" \
  npm run benchmark:terminal --prefix firecrab-e2e
```

`FIRECRAB_TERMINAL_BENCH_OUTPUT` is resolved from the command's working directory;
use an absolute path to keep the output location independent of npm's prefix.
Use `-- --profile-resize` to instrument synchronous resize components at device
scale factors 1 and 2. Instrumented timings are separate from benchmark samples.

Use `FIRECRAB_TERMINAL_BENCH_OUTPUT` to select another directory.
`FIRECRAB_TERMINAL_BENCH_SAMPLES` defaults to seven and must be at least three.
Every measured workload has two preceding warmups. Engine order alternates.

## Workloads and measurement boundaries

Both engines use the same font stack, 13px font size, 5000-line history limit,
80×24 and 160×48 grids, and 4093-byte chunks. The odd chunk size deliberately
splits UTF-8 characters and escape sequences. Workload construction is excluded
from timing. Fixtures cover plain logs, ANSI colors, Korean/combining marks/emoji,
ASCII soft wrapping, and 600 full-screen TUI updates.

The final screen text and cursor must match an independently constructed
expectation. Incorrect samples make the command fail, even when faster.

Output timing starts at enqueue and ends after the final marker reaches the
parsed buffer and a following animation frame. Write callbacks are not the
boundary because the libraries schedule them differently. This is a rendering
opportunity proxy, **not** a GPU presentation measurement or parser-only score.
Synchronous enqueue time and Chromium long tasks are also recorded.

Input is a synthetic keydown with local echo and two following animation frames.
It excludes hardware keyboard, OS IME, network, and guest latency. Fresh-context
startup includes lazy engine loading, runtime initialization, opening, and two
frames; it excludes initial harness navigation. A fresh context does not imply
a cold browser process, OS cache, or network download over a real connection.

Resize timing covers 40 local grid changes. It does not change the guest tty's
dimensions. Automatic guest resize requires a separate console protocol design.
An additional paced case changes the grid once per frame over 40 frames. It
reports wall time and handler time separately, because deferring drawing moves
work out of the synchronous handler without eliminating that frame's drawing.

The memory observations compare Chromium's JS heap and backing storage before
and after ten mount/output/dispose cycles with requested garbage collection.
They do not establish peak process memory or prove absence of leaks.

## Compatibility findings

The stable 0.4.0 package needs three local integration accommodations:

1. Its custom keyboard callback consumes `true`; xterm consumes `false`.
   The IME/key adapter accounts for this difference.
2. Freeing a terminal that displayed complex graphemes can corrupt a shared
   WASM instance ([upstream #141](https://github.com/coder/ghostty-web/issues/141)).
   Every Firecrab terminal gets a separate WASM instance. An instance is never
   reused after disposing its terminal. Startup measurements include this cost.
3. Its compatibility buffer truncates multi-codepoint graphemes
   ([upstream #200](https://github.com/coder/ghostty-web/issues/200)). Screen text
   and log export use the public native grapheme APIs. `grapheme_len` counts
   additional codepoints, excluding the base character.

The installed npm package remains unchanged. A Vite plugin applies two checked
transformations to the pinned distribution during both development and builds.
It fails explicitly if the expected source changes. MIT notices ship in the
dashboard's `terminal-licenses.txt` asset.

## Resize and payload analysis

The original 40-resize diagnostic at device scale 1 spent approximately 1.5 ms
inside core resize calls, 8.6 ms inside the Canvas sizing method, and 129 ms
inside immediate full rendering. These are instrumented observations, not a
statistical comparison. Nested render/sizing times must not be added together.

The upstream terminal resize method sizes the Canvas twice and renders every
intermediate grid synchronously. At device scale 2, its second sizing step also
removes the renderer's scaled backing dimensions, causing render to size it a
third time. Assigning Canvas width or height resets the context even when the
value is unchanged ([Canvas width behavior](https://developer.mozilla.org/en-US/docs/Web/API/HTMLCanvasElement/width)).

The build transformation keeps every WASM resize and resize event synchronous.
It lets the existing render loop size and draw the final dirty state next frame.
This preserves grid/reflow transitions while avoiding intermediate pictures that
the browser cannot present within one synchronous task. Device-scale-2 browser
tests verify the backing dimensions, transform and nonblank text after resizing.

The published JavaScript contains 564,060 Base64 characters representing the same
423,045-byte WASM file included in the package. The build plugin verifies the two
artifacts are byte-identical and replaces the inline literal with a Vite asset
import. The external WASM gets a content-hashed filename. The WASM binary itself
is unchanged; it has no removable custom/debug sections. The JavaScript engine
chunk drops from about 636 kB to 72 kB before compression. Compare total JS **and**
WASM payload in the report, rather than quoting the JS reduction alone.

The 2026-10-23 same-harness comparison reduced median 40-call burst resize from
140.1 ms to 16.6 ms, and total computed gzip payload from 188,624 to 147,875 bytes.
Paced resize wall time stayed near 349 ms for 40 frames; its synchronous handler
p95 fell from 5.2 ms to 0.1 ms. Both runs passed all 140 output checks. The current
browser suite passed 40 tests, including the high-DPI test, with one guest test
skipped. These replace the original numbers below for assessing the optimization.

## Validation and rollout limits

```sh
FIRECRAB_MOCKED_E2E_BASE_URL=http://127.0.0.1:18238 \
  npm run test:mocked --prefix firecrab-e2e -- \
  tests/terminal-engines.spec.ts tests/console-session.spec.ts \
  tests/ssh-console.spec.ts tests/dashboard-frame.spec.ts tests/command-palette.spec.ts
npm run build --prefix firecrab-frontend
npm run lint --prefix firecrab-frontend
```

The mocked suite checks split UTF-8, combining/ZWJ text export, keyboard input,
composition commit, shortcut isolation, initialization failure, repeated sessions,
session end/reconnect, and console layout/settings. A synthetic composition event
does not replace testing real OS IMEs. The real-guest console test remains
separate and is skipped by the mocked configuration.

Measurements on one local machine are informational. A production rollout still
needs real guest/TUI sessions, actual Korean IMEs, Safari/Firefox and mobile input,
and repeated measurements on representative hardware. No CI timing threshold is
set from a single run.

## Original result: 2026-10-23, before optimization

See [the original report](../../benchmarks/terminal/before-optimization/report.md)
and [original raw samples](../../benchmarks/terminal/before-optimization/results.json).

On Apple M5 / macOS arm64 / headless Chromium 151.0.7922.34, all 140 measured
output samples matched expected screen text and cursor position. The related
mocked browser suite passed 39 tests; its one real-guest test was skipped.

| Metric | xterm 6.0.0 | ghostty-web 0.4.0 |
|---|---:|---:|
| 160×48 ANSI output, median | 25.0 ms | 16.7 ms |
| 160×48 TUI updates, median | 25.0 ms | 22.5 ms |
| Fresh-context initialization, median | 19.1 ms | 19.2 ms |
| Synthetic local echo, p95 | 16.8 ms | 17.1 ms |
| 40 local resizes, median | 16.5 ms | 141.6 ms |
| Harness resources, computed gzip | 87,389 bytes | 188,099 bytes |
| JS heap change after 10 lifecycle cycles | +15,608 bytes | +1,403,416 bytes |

This run favors Ghostty for some burst-output workloads. Small workloads and
input remain close to the animation-frame floor. Resize cost and payload are
clear regressions. Ghostty's synchronous enqueue reached 16.7 ms while xterm
enqueue stayed at or below 0.1 ms; the end-to-frame proxy alone does not describe
main-thread responsiveness. No measured output task exceeded Chromium's 50 ms
long-task threshold.

The JS heap delta is an observation after requested garbage collection, not a
confirmed leak. The result supports continued prototype work, with resize,
payload, and lifecycle memory as the next investigation targets.
