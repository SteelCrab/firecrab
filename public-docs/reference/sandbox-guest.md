# Sandbox guest channel V1

The controller authenticates with the VM's existing operator SSH key. It pins
the VM's expected host key and the admitted runtime UUID. It disables SSH agent
forwarding, user SSH configuration, password prompts and forwarding. Provider
credentials and controller environment variables never enter the guest.

## Command contract

`SandboxGuestCommand` has `version: 1`, `argv`, relative `cwd`, explicit `env`,
UTF-8 `stdin` and `limits`. Arguments are transported as literal strings.
Shell operators inside an argument are not evaluated. The executable cannot
begin with `-` or contain `=`. NUL is rejected in argv, cwd and environment.
Stdin can contain NUL and is limited to 64 KiB.

The guest resolves cwd inside its owned workspace and checks the physical path.
Parent traversal, absolute paths and escaping symlinks are rejected. The command
receives an empty inherited environment plus HOME, PATH, explicit values and
authorized environment service-discovery values. It runs as a dedicated UID
with no supplementary groups and no-new-privileges enabled.

| Bound | Accepted values |
| --- | --- |
| argv | 1–128 arguments; 8192 total bytes |
| environment | 64 entries; 16384 total bytes |
| cwd | Relative; at most 1024 bytes |
| wallSeconds | 1–120 |
| cpuSeconds | 1–120 |
| memoryMib | 16–65536 address-space limit |
| outputBytes | 1–65536 per stream |
| Open files | Fixed at 1024 per process, including direct/Docker/nspawn payloads |

The channel returns stdout and stderr separately. It records the invocation
identity and confirmed exit status independently of the SSH connection's exit.
Output overflow stops owned processes and returns exit 125. A lost response
requires reconciliation. It never silently repeats a command. Reconciliation
recovers bounded output and a known exit when available before guest cleanup.
A guest reboot may leave partial output without a confirmed exit.

Native signal exits are preserved. For example, BusyBox hard-KILL deadlines can
return 137 and confirmed cancellation can retain 143. An exit number alone does
not identify its cause. Check the requested limits, observed duration, explicit
cancellation action and exact process identity together.

## Scoped controls

The following endpoints resolve an already-admitted task under
`/api/ai-sandbox/projects/PROJECT/tasks/TASK/guest`:

| Request | Behavior |
| --- | --- |
| `GET /health` | Probe V1, detected init, execution and container-engine capabilities. |
| `GET /processes` | Inspect only the attempt's dedicated UID. |
| `POST /signal` | Send TERM or KILL to an exact owned boot/PID/start-time tuple. |

Signals require task execution authority. They reject a different UID, guest
boot or recycled PID. Cancellation uses the existing task cancellation API.
The controller fences the attempt before stopping its root supervisor and UID.
Docker cancellation also verifies its immutable attempt ownership label before
removing the exact container. It never removes an image or unrelated container.

## Guest capabilities

The V1 execution transport requires a shell, operator SSH, Linux `/proc`,
tar with excludes, timeout and hard ulimit support. UID switching and timed
locking use util-linux or the shipped static helper. The helper is never setuid.
It rejects non-root supervisors, root task identities and unbounded lock waits.
These capabilities are probed before staging. systemd and OpenRC init detection
are separate from execution support. Prepared distroless images use the static
helper, injected BusyBox applets and key-only SSH overlay described in the
[acceptance recipe](../development/sandbox-environments-qa.md). Missing tools
report unsupported execution. An init name alone never proves support.
Docker/nspawn also require cgroup v2.

V1 provides bounded command execution and scoped process controls. Persistent
interactive PTY create/attach/input/output/resize/reconnect belongs to Runtime
and the later Canvas integration.
