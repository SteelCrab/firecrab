# Guest console dimensions

The browser console uses the Ghostty WASM core. A separate control path updates
the guest's ttyS0 dimensions so foreground programs receive SIGWINCH and redraw.
The shell remains the same shared serial session; no independent PTY is created.

## Requirements

- Linux host and a Firecracker guest kernel with `CONFIG_VIRTIO_VSOCKETS=y`,
  or an available virtio-vsock module.
- A complete host installation containing `firecrab-guest-console` beside the API.
- VM stop/start after upgrading: specialization writes the helper into the guest.

The static musl helper runs in glibc, musl and minimal images. The existing metrics
service starts it on systemd, OpenRC and BusyBox paths. It has no SSH, IP network
or additional guest-package dependency. Unsupported guests keep their serial
console and show `Guest size not synchronized`.

Older self-updaters do not install newly introduced bundle members. When upgrading
from one of those versions, rerun the complete host installer before restarting VMs.
Current installers and updaters include the helper and its musl attribution.
An explicit `FIRECRAB_GUEST_CONSOLE_BIN` selects a local build for API development.

## Shared grid

Each modern viewer reports its visible local viewport after fitting, font changes
and reconnect. Measurements below 16 pixels are ignored. The shared grid uses the
minimum columns and minimum rows across connected modern viewers, independently.
Every viewer renders the acknowledged common grid, even in a larger window.
Closing a viewer removes its constraint. With no viewers, the last guest size stays
in effect. A new connection starts a fresh size negotiation.

The host coalesces changes for 50 ms. Only one live viewer's worker performs guest
I/O. If that viewer disconnects, another worker takes over. Obsolete replies cannot
confirm a newer target. Guest I/O has a 750 ms deadline and retries after 3 seconds;
it does not block console output or keyboard input.

## WebSocket protocol

A browser requests the `firecrab.console.v1` subprotocol on the existing console URL.
Input and guest output remain binary frames. Text frames carry control JSON only:

```json
{"type":"resize","rows":40,"cols":120}
```

Dimensions must be integers from 2 through 1000. Control frames are at most 128 bytes.
Malformed or unknown controls close only that viewer with code 1008 and never reach
the shell. Clients without the subprotocol retain the existing raw text/binary input.

The server sends an initial status before replay, and updates it after changes:

```json
{"type":"resize","status":"applied","size":{"rows":40,"cols":120},"viewers":2}
```

Status is `pending`, `applied` or `unavailable`. `size` is the last acknowledged
size, or null before the first acknowledgement. A failed request must not claim
that the guest applied the requested dimensions.

## Host and guest protocol

Every VM start has its own `v.sock` in its private runtime directory. This prevents
connections from a previous VM generation from reaching a replacement instance.
Firecracker is configured with guest CID 3 and that socket path, including VMs
without a network interface.

The host connects to this Unix socket and sends `CONNECT 47100\n`. Firecracker
answers `OK <host-port>\n` before forwarding the connection to the guest listener.
This follows [Firecracker's vsock protocol](https://github.com/firecracker-microvm/firecracker/blob/main/docs/vsock.md).

The guest accepts only peer CID 2 (the host). Each connection carries one 8-byte
request: ASCII `FCR1`, then unsigned 16-bit big-endian rows and columns. It applies
`TIOCSWINSZ` to `/dev/ttyS0` opened with `O_NOCTTY`. The kernel sends SIGWINCH to
the foreground process group. The reply echoes dimensions after `FCA1`, or returns
`FCE1` followed by a 32-bit big-endian errno. There is no command-execution operation.
One-second read/write timeouts bound partial or stalled requests.

## Build and validate

On Linux, build the static helper before a source API run:

```sh
sudo apt-get install musl-tools
./scripts/build-guest-console.sh target/debug
cargo build -p firecrab-api --locked
```

Host releases build the helper on x86_64 and aarch64 for both host libc variants.
Both installers preserve its executable mode and attribution. Source development
staging and self-update rollback include the same files.

Native PTY and protocol checks:

```sh
python3 scripts/test-guest-console.py
cargo test -p firecrab-api --locked console
cargo test -p firecrab-api --locked guest_console
```

The C PTY tests also run on macOS. They check split frames, invalid bounds, partial
requests, actual dimensions and foreground SIGWINCH delivery. Rust tests check the
shared minimum, disconnect, stale replies, handshake, coalescing and cleanup.

Browser checks:

```sh
FIRECRAB_MOCKED_E2E_PRODUCTION=1 FIRECRAB_MOCKED_E2E_CROSS_BROWSER=1 \
  npm run test:mocked --prefix firecrab-e2e -- tests/wasm-terminal.spec.ts
```

The booted-guest console check additionally compares `stty size` with both browser
viewers, observes the foreground shell's WINCH trap and resizes BusyBox vi and top:

```sh
FIRECRAB_E2E_REQUIRE_GUEST_RESIZE=1 npm run test:console --prefix firecrab-e2e
```

It requires the existing Linux/KVM guest-test setup and the installed helper.
Installer CI enables this requirement; mocked browser fixtures do not prove guest integration.
