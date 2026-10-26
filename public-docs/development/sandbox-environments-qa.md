# Environment and guest execution acceptance

Run live checks serially on an operator-approved host. Do not run the root-only
shell fixture on the API host. Do not reuse a production Workspace. Retain the
manifest, project/environment/generation/role/VM/runtime/task/attempt identities,
typed requests, actual stream bytes, exit status and final resource inventory.
Provider, MicroVM, container and public-network evidence are separate records.

## Deterministic checks

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test -p firecrab-api ai_sandbox::environment
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test -p firecrab-api ai_sandbox::guest
python3 scripts/test-guest-console.py
sudo python3 scripts/test-sandbox-user.py
```

For a small Linux tooling proof without rebuilding Rust, mount the repository
read-only into an owned disposable root container. Mount the compiled helper at
`/usr/local/sbin/firecrab-console-resize` and set
`FIRECRAB_SANDBOX_GUEST_QA=1`. Run
`python3 scripts/test-sandbox-guest-shell.py`. It executes the production shell
with the distribution UID/timed-lock utilities disabled. It verifies actual
static fallback, separate output/exit, bounded output, scoped signal refusal,
cancellation, recovery, export, Workspace hydrate/overlay/delete, private
retention and a final file-tool flush. Its output explicitly identifies it as
a Linux process fixture and sets `microvmValidated: false`.

The last command compiles and executes the real C helper on Linux. It changes
UID only in child processes and creates only a private temporary lock file.
It verifies exact UID/GID, empty supplementary groups, no-new-privileges, separate
output, exit status, root-target rejection, unprivileged-caller rejection and a
bounded lock retained by the supervisor's open file description. macOS reports
these Linux tests as skipped.

Build the static helper with `scripts/build-guest-console.sh target/ingress`.
The existing release bundle installs it through the guest console installer.
`scripts/test-ai-sandbox-guest.sh API_TEST_BINARY PROFILE_DIRECTORY` mounts this
helper into its owned root container. Its ignored tests include Workspace
hydrate → host edit/delete → refresh, excluded user-package/config preservation,
bounded output, cancellation, export and cleanup. These remain Linux-process
fixtures, not live MicroVM acceptance.

## systemd and OpenRC guests

1. Select an installed Ubuntu/systemd image pin and an Alpine/OpenRC image pin
   from the project's `environment-images` endpoint. Use one small role per
   environment while validating OS behavior. Declare `persistent_workspace` and
   no TTL. Record the independent template version and all hashes.
2. PUT the manifest, then reconcile until agent/network/application are ready.
   The application check may pass without a declared required service. It does
   not prove that a separate application is running.
3. Admit an authorized real model task against that environment and role.
   Use `run_argv` with `/bin/sh`, `-c`, and an explicit program that copies stdin,
   prints one stdout/stderr nonce, reports UID and `NoNewPrivs`, then exits 7.
   Verify the separate streams and recorded exit 7 from the actual guest.
4. Request a wall timeout, output overflow and cancellation in separate attempts.
   Verify no live processes remain for the recorded UID. Query processes and
   refuse a signal with a modified boot/start identity. Do not use unrelated PIDs.
5. Create `main.txt`, an allowed `config/tool.conf`, and excluded `.local` and
   `.config` files in the Workspace. Stop and resume the environment. On the next
   attempt, verify the same disk data, then edit `main.txt` and delete
   `config/tool.conf` through model tools before the first command. Verify the
   guest receives those changes and preserves the excluded package/config files.
6. Restart the API. Reconcile the environment and recorded attempts. Assert that
   the reservation and VM UUIDs are unchanged. Export, then show that DELETE
   without `authorizeWorkspaceDeletion` refuses. Delete only after explicit
   authorization and successful result/artifact verification.

## Prepared distroless guest

An unprepared distroless image may have no SSH server, UID utility or timed
flock. A capability failure alone does not pass distroless acceptance.

The V1 channel reuses the injected static BusyBox runtime and the shipped static
guest helper. The helper provides non-root UID/GID switching with
no-new-privileges and a bounded inherited-descriptor lock. It is mode 0755,
never setuid. Only the authenticated root supervisor can invoke these operations.
The OCI and offline-disk preparation paths expose missing BusyBox applets without
replacing existing distribution binaries.

`packaging/sandbox/distroless.Dockerfile` supplies a small SSH overlay. Build it
in a private context containing that Dockerfile and the static
`firecrab-guest-console` binary. Set APP_IMAGE to the intended application's
digest and SSH_TOOLS_IMAGE to a digest-pinned Alpine image of the same
architecture. The SSH executable, loader and dependencies live under
`/opt/firecrab/ssh-runtime`. The original application files and image metadata
remain the base. No SSH host key, operator private key or provider credential
belongs in the build context. The normal VM specialization injects per-VM keys.
Capture the resulting image digest and import it through the existing OCI API.

Build dependencies are obtained during preparation; the final imported digest
is the environment pin. Rebuild the prepared image when changing the SSH tool
chain. Verify key-only SSH on the prepared image before model admission. A locked
root account must be prepared for key-only operator access through the normal
image build process. Never enable password or interactive authentication.

Repeat the six OS steps above. `guest/health` must report `init: distroless`,
`execution: true` and protocol 1. Retain the actual static helper probe and
UID/no-new-privileges evidence. Removing the helper or SSH server from a separate
disposable image must produce an explicit unsupported/channel error. Never
remove tools from a running retained Workspace to test the failure path.

## Docker and nspawn modes

Run each engine independently inside one managed MicroVM. Install and pin the
engine's test image before the environment becomes ready. Docker requires
cgroup v2 with memory and pids controllers. nspawn requires systemd and cgroup
v2. Do not expose the guest Docker socket to the task UID.

For Docker, use `agent_sandbox`, `runtime: docker`, and an installed
`repository@sha256:digest` image containing the V1 shell tools. The trusted root
wrapper starts the engine with a fixed entrypoint, unprivileged UID, dropped
capabilities, no-new-privileges, memory/CPU/pids/file limits and owned mounts.
Its `--network host` means the managed guest's network. The controller host is
not the Docker host. The model payload is a read-only command file; it cannot
become engine flags.

For nspawn, export a trusted root tree as an archive. SHA256 the archive and
install the root-owned regular file at
`/var/lib/firecrab-sandbox-images/DIGEST.tar` inside the managed guest. Select
`runtime: nspawn` and `image: sha256:DIGEST`. The root wrapper verifies its bytes,
extracts a private attempt copy, creates the attempt account in that private
copy, and applies nspawn resource/capability restrictions. The shared archive is
never modified or removed.

For each engine, repeat execution, separate streams, exit, output overflow,
cancellation, required artifact export and TTL cleanup. Fail one required export
and verify that the VM, root data and result records remain. Then repair/export
and verify that cleanup removes only the recorded owned IDs. Inspect the exact
Docker attempt label/name, or nspawn's owned private root, before and after.
Verify the shared image/base archive remains. Preserve results and errors when
the selected engine/image is unavailable. Engine fixture results do not prove
that this sequence ran inside a MicroVM.

## Four-role policy and recovery

Provision Debug, Test, Backend and DB from one manifest on a dedicated IPv4-only
subnet. Pin images whose DB and Backend services start without task side effects.
Declare Backend TCP 8080 and DB TCP 5432. Allow only Debug/Test → Backend:8080 and
Backend → DB:5432. Use service-discovery values from the environment response or
the injected `FIRECRAB_SERVICE_*` values. Host-originated operator SSH/readiness
and Nginx traffic are outside VM forward-chain policy; do not add broad guest
ingress rules to enable probes.

Measure both allowed connections and refused Debug/Test → DB, DB → Internet,
other-environment destinations, IPv6 global and link-local attempts. Repeat with
two environments from the same seed image. Record equal initial seed rows and
independent writes. Interrupt provisioning after one resource creation, restart
the API and reconcile. Assert one network/VM/SG per reserved ID and no duplicate
rows or effects. A rendered policy or fixture does not pass these packet checks.

Attach live output and final REST inventories to the QA record. Leave every
unexecuted OS, engine, network or recovery scenario explicitly pending.
