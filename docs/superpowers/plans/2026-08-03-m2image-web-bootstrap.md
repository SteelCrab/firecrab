# M2Image 웹 부트스트랩 빌드 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the web dashboard bootstrap Alpine/Ubuntu/Rocky from official upstream sources itself — inside a builder microVM, no docker, no sudo, no new privileged process — producing a `.tar.zst` package that the existing "가져오기" install pipeline (`image_install.rs`) picks up unchanged.

**Architecture:** Reuse the builder-VM mechanics `handlers::builds` already established (`create_vm`/`start_vm_request`/console/`delete_vm`, `VmPurpose::Builder`), but as a new, separate session kind (`handlers::bootstrap`, `BootstrapTracker`) since the state machine and terminal action are genuinely different: instead of installing named packages onto the VM's own running distro and registering its disk as a new template version, this pushes a whole bootstrap script (download official base → chroot in → install packages + kernel via the target's own package manager → `mkfs.ext4 -d` a finished rootfs file) over the console, then dumps the resulting rootfs/kernel/initrd files out of the builder VM's disk (via a new debugfs-based `dump_from_image`, the read counterpart to the existing `write_into_image`) and packages them into `{alias}.tar.zst` at the exact path `image_install::staged_package_path` already reads from.

**Tech Stack:** Rust (axum, tokio, e2fsprogs' `debugfs`/`mkfs.ext4`/`e2fsck`), POSIX shell (guest-side bootstrap scripts), React 19 + TypeScript, hand-maintained `firecrab-frontend/src/bindings/*.ts`.

## Global Constraints

- `firecrab-api` stays a non-privileged process — no new root daemon, no docker dependency (`packaging/systemd/firecrab-api.service` keeps `NoNewPrivileges=yes`/`ProtectSystem=full`). This plan adds zero new host-side privileged code — every privileged operation (chroot, mount, mkfs) happens inside the builder guest, which is already root in its own isolated kernel.
- Only the 3 built-in aliases (`alpine-3.24`, `ubuntu-26.04`, `rocky-9`) are in scope. Adding a brand-new distro family is out of scope (needs a new hardcoded `TemplateSpec` in `templates.rs::default_specs()`).
- Bootstrapping a target requires at least one template already installed to serve as the builder VM's own OS — this plan cannot produce the very first installed image on a completely empty host. That first image still comes from `scripts/build-m2images.sh` (CLI) or a pre-published package via `FIRECRAB_IMAGE_BASE_URL`.
- Only one bootstrap session may run at a time (a second `POST` while one is active gets `409`).
- Every artifact this feature produces must land at the exact relative paths `firecrab-api/src/templates.rs::default_specs()` already hardcodes for that alias, so the unmodified "가져오기" pipeline (`image_install.rs::install_staged_package_once`) accepts it without any changes to that file.
- Rust: `rustfmt`/`clippy` clean, existing test patterns reused (`tokio::test`, `handlers::vms::test_support::{test_state, record, seed_vm}`, `handlers::builds`'s local `register_fake_process`/`wait_for_console_subscriber` pattern).
- Frontend: no test framework — verified by `tsc -b` (`npm run build`) and manual browser check.
- UI copy stays Korean; no "Packer" branding.
- Follow existing doc-comment style: explain *why*, not *what*.

---

## File Structure

**Backend (`firecrab-api/`, `firecrab-api-types/`):**
- `firecrab-api-types/src/lib.rs` — add `BootstrapStatus`, `BootstrapResponse`
- `firecrab-api/src/rootfs.rs` — add `dump_from_image` (debugfs-based file extraction, no mount)
- `firecrab-api/src/handlers/packages.rs` — widen `wait_for_completion`/`find_done_sentinel`/`OUTPUT_TAIL_CAP`/`DONE_SENTINEL`-adjacent constants to `pub(crate)` so `handlers::bootstrap` can reuse the console-sentinel-wait mechanics instead of duplicating them
- `firecrab-api/src/handlers/builds.rs` — widen `builder_vm_name`, `builder_micro_network_id`, `mark_as_builder` to `pub(crate)` so `handlers::bootstrap` reuses them instead of duplicating VM-creation plumbing
- `firecrab-api/src/bootstrap.rs` — new: `BootstrapTracker` (mirrors `BuildTracker`)
- `firecrab-api/src/handlers/bootstrap.rs` — new: `start_bootstrap`, `watch_bootstrap_boot`, `run_bootstrap_script`, `package_bootstrap`, `get_bootstrap`, `cancel_bootstrap`
- `scripts/firecracker-menual/bootstrap-alpine-in-guest.sh`, `bootstrap-ubuntu-in-guest.sh`, `bootstrap-rocky-in-guest.sh` — new, run entirely inside the builder guest
- `firecrab-api/src/state.rs` — add `bootstraps: crate::bootstrap::BootstrapTracker` to `AppState`
- `firecrab-api/src/main.rs` — `mod bootstrap;`
- `firecrab-api/src/handlers/mod.rs` — `pub mod bootstrap;`
- `firecrab-api/src/server.rs` — route wiring

**Frontend (`firecrab-frontend/src/`):**
- `bindings/BootstrapStatus.ts`, `bindings/BootstrapResponse.ts` — new
- `bindings/index.ts` — export additions
- `api/client.ts` — `startBootstrap`, `getBootstrap`, `cancelBootstrap`
- `components/Images.tsx` — "부트스트랩 빌드" button + log panel per row

**Docs:**
- `docs/superpowers/specs/2026-08-03-m2image-web-rebuild-design.md` — already written (this plan implements it)
- `docs/20-guides/m2image-builder.md` — add a section documenting the new capability

---

### Task 1: Wire types — `BootstrapStatus`, `BootstrapResponse`

**Files:**
- Modify: `firecrab-api-types/src/lib.rs`
- Create: `firecrab-frontend/src/bindings/BootstrapStatus.ts`, `firecrab-frontend/src/bindings/BootstrapResponse.ts`
- Modify: `firecrab-frontend/src/bindings/index.ts`

**Interfaces:**
- Produces: `firecrab_api_types::{BootstrapStatus, BootstrapResponse}`, consumed by every later Rust task in this plan and by Task 10 (frontend bindings mirror them 1:1).

- [ ] **Step 1: Add the Rust types**

In `firecrab-api-types/src/lib.rs`, near `BuildStatus`/`BuildResponse`:
```rust
/// Lifecycle of one from-scratch distro bootstrap session (`handlers::bootstrap`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapStatus {
    /// Builder VM is being created/started.
    Booting,
    /// The bootstrap script (download + chroot install + mkfs) is running
    /// on the builder VM's console.
    Running,
    /// VM stopped; extracting rootfs/kernel/initrd from its disk and
    /// packaging them into `{alias}.tar.zst`.
    Packaging,
    /// Package written to the local install cache; builder VM deleted.
    Succeeded,
    /// Failed at any stage; see `log`. Builder VM has been deleted.
    Failed,
}

/// Status + log for one bootstrap session
/// (`POST /api/images/{alias}/bootstrap`, `GET`/`DELETE /api/images/bootstrap/{bootstrapId}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapResponse {
    pub bootstrap_id: Uuid,
    /// The target being bootstrapped (`alpine-3.24`, `ubuntu-26.04`, or `rocky-9`).
    pub alias: String,
    /// Which already-installed template's VM is doing the work — an
    /// unrelated, disposable environment, not the bootstrap's target.
    pub source_alias: String,
    /// Builder VM id, so the dashboard can reuse the existing console
    /// WebSocket (`/ws/vms/{id}/console`) to show live output.
    pub vm_id: Uuid,
    pub status: BootstrapStatus,
    pub log: String,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
}
```
(`Uuid` is already imported in this file.)

- [ ] **Step 2: Add TS bindings**

`firecrab-frontend/src/bindings/BootstrapStatus.ts`:
```ts
// Mirrors firecrab_api_types::BootstrapStatus (snake_case wire shape).

export type BootstrapStatus = "booting" | "running" | "packaging" | "succeeded" | "failed";
```

`firecrab-frontend/src/bindings/BootstrapResponse.ts`:
```ts
// Mirrors firecrab_api_types::BootstrapResponse (camelCase wire shape).

import type { BootstrapStatus } from "./BootstrapStatus";

export type BootstrapResponse = {
  bootstrapId: string;
  alias: string;
  sourceAlias: string;
  vmId: string;
  status: BootstrapStatus;
  log: string;
  startedAtMs: number;
  endedAtMs?: number;
};
```

Add to `firecrab-frontend/src/bindings/index.ts`:
```ts
export * from "./BootstrapStatus";
export * from "./BootstrapResponse";
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p firecrab-api-types`
Run: `cd firecrab-frontend && npx tsc --noEmit`
Expected: both clean (nothing references the new types yet).

- [ ] **Step 4: Commit**

```bash
cargo fmt -p firecrab-api-types
git add firecrab-api-types/src/lib.rs firecrab-frontend/src/bindings/BootstrapStatus.ts \
  firecrab-frontend/src/bindings/BootstrapResponse.ts firecrab-frontend/src/bindings/index.ts
git commit -m "feat: add bootstrap session wire types"
```

---

### Task 2: `dump_from_image` — extract a file from an ext4 image without mounting

**Files:**
- Modify: `firecrab-api/src/rootfs.rs`

**Interfaces:**
- Consumes: `run_debugfs` (existing, private `fn` in the same file — reused directly).
- Produces: `pub fn dump_from_image(rootfs: &Path, guest_path: &str, dest: &Path) -> Result<(), RootfsError>` — used by Task 8 (`handlers::bootstrap::package_bootstrap`) to pull the finished rootfs/kernel/initrd files out of the builder VM's own disk.

- [ ] **Step 1: Write the failing test**

Add to `rootfs.rs`'s `#[cfg(test)] mod tests` (reuse this file's existing ext4 fixture helper — check for whatever helper `specialize_guest`'s/`finalize_template_disk`'s own tests use to build a test ext4 image, e.g. `make_test_ext4...`, and reuse it rather than duplicating):
```rust
#[test]
fn dump_from_image_extracts_a_file_written_earlier() {
    let (_dir, rootfs) = make_test_ext4_with_etc(); // reuse existing helper
    write_into_image(&rootfs, "/etc/payload", b"hello from the guest disk\n").unwrap();

    let dest_dir = tempdir().unwrap();
    let dest = dest_dir.path().join("payload.out");
    dump_from_image(&rootfs, "/etc/payload", &dest).unwrap();

    assert_eq!(fs::read(&dest).unwrap(), b"hello from the guest disk\n");
}

#[test]
fn dump_from_image_fails_clearly_for_a_missing_guest_path() {
    let (_dir, rootfs) = make_test_ext4_with_etc();
    let dest_dir = tempdir().unwrap();
    let dest = dest_dir.path().join("missing.out");

    let error = dump_from_image(&rootfs, "/etc/does-not-exist", &dest);

    assert!(error.is_err());
}
```
(If `make_test_ext4_with_etc` doesn't exist under that exact name, use whichever helper `finalize_template_disk`'s own tests actually call — check the file for it first.)

Run: `cargo test -p firecrab-api rootfs::tests::dump_from_image -- --nocapture`
Expected: FAIL (function doesn't exist).

- [ ] **Step 2: Implement `dump_from_image`**

Add near `write_into_image`:
```rust
/// Extracts `guest_path` from `rootfs`'s ext4 image to `dest` on the host,
/// without mounting — the read counterpart to [`write_into_image`]. Used to
/// pull a bootstrap builder's freshly-assembled target rootfs/kernel/initrd
/// files out of its own disk once the guest-side script has finished
/// building them (`handlers::bootstrap::package_bootstrap`). Works for
/// files of any size `debugfs`'s `dump` command supports — the filesystem
/// itself is the only real limit, unlike `write_into_image`'s small
/// identity files.
pub fn dump_from_image(rootfs: &Path, guest_path: &str, dest: &Path) -> Result<(), RootfsError> {
    let output = run_debugfs(rootfs, &format!("dump {guest_path} {}", dest.display()))?;

    // debugfs's own exit code doesn't reliably reflect whether `dump` found
    // the path (same caveat as `write_into_image`), so success is confirmed
    // positively: a real dump produces a non-empty file at `dest`.
    match fs::metadata(dest) {
        Ok(metadata) if metadata.len() > 0 => Ok(()),
        _ => {
            let _ = fs::remove_file(dest);
            Err(RootfsError::Specialize {
                path: rootfs.to_owned(),
                detail: format!(
                    "debugfs did not produce a non-empty {} for {guest_path}: {}",
                    dest.display(),
                    String::from_utf8_lossy(&output.stderr)
                ),
            })
        }
    }
}
```

- [ ] **Step 3: Run tests, verify pass**

Run: `cargo test -p firecrab-api rootfs::tests::dump_from_image -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
git add firecrab-api/src/rootfs.rs
git commit -m "feat: add dump_from_image to extract files from a guest disk without mounting"
```

---

### Task 3: Widen visibility for reuse — console-wait mechanics + builder-VM helpers

**Files:**
- Modify: `firecrab-api/src/handlers/packages.rs`
- Modify: `firecrab-api/src/handlers/builds.rs`

**Interfaces:**
- Produces: `pub(crate)` visibility on `packages::{wait_for_completion, find_done_sentinel, OUTPUT_TAIL_CAP}` and `builds::{builder_vm_name, builder_micro_network_id, mark_as_builder}` — consumed by Task 5/7 (`handlers::bootstrap`).

This is a pure mechanical visibility change — no behavior change, no new tests needed beyond confirming the existing test suites still pass (a `pub(crate) fn` is still callable everywhere a private one was).

- [ ] **Step 1: Widen `packages.rs`**

In `firecrab-api/src/handlers/packages.rs`, change:
```rust
async fn wait_for_completion(
```
to:
```rust
pub(crate) async fn wait_for_completion(
```

Change:
```rust
fn find_done_sentinel(buffer: &[u8]) -> Option<i32> {
```
to:
```rust
pub(crate) fn find_done_sentinel(buffer: &[u8]) -> Option<i32> {
```

Change:
```rust
const OUTPUT_TAIL_CAP: usize = 8 * 1024;
```
to:
```rust
pub(crate) const OUTPUT_TAIL_CAP: usize = 8 * 1024;
```

`DONE_SENTINEL` itself (`"FIRECRAB_PKG_UPDATE_DONE"`) stays private and unused by `handlers::bootstrap` — the bootstrap module defines its own distinct sentinel constant (Task 7), since a bootstrap script's completion is a conceptually different event from a package action's.

- [ ] **Step 2: Widen `builds.rs`**

In `firecrab-api/src/handlers/builds.rs`, change:
```rust
fn builder_vm_name(alias: &str) -> String {
```
to:
```rust
pub(crate) fn builder_vm_name(alias: &str) -> String {
```

Change:
```rust
async fn builder_micro_network_id(state: &AppState, request_id: Uuid) -> Result<Uuid, AppError> {
```
to:
```rust
pub(crate) async fn builder_micro_network_id(state: &AppState, request_id: Uuid) -> Result<Uuid, AppError> {
```

Change:
```rust
async fn mark_as_builder(state: &AppState, vm_id: Uuid, request_id: Uuid) -> Result<(), AppError> {
```
to:
```rust
pub(crate) async fn mark_as_builder(state: &AppState, vm_id: Uuid, request_id: Uuid) -> Result<(), AppError> {
```

Update `builder_vm_name`'s doc comment (currently says "not shown anywhere in the dashboard since `list_vms` filters `Builder` records out" — still true, no change needed there) — but its first line currently reads as build-session-specific; broaden it slightly:
```rust
/// Names a builder VM so it's recognizable if an operator inspects
/// `data/firecrab.db` directly; not shown anywhere in the dashboard since
/// `list_vms` filters `Builder` records out. Shared by `handlers::builds`
/// (package-customization sessions) and `handlers::bootstrap` (from-scratch
/// distro sessions) — both tag their VM `VmPurpose::Builder` the same way.
```

- [ ] **Step 3: Confirm nothing broke**

Run: `cargo check -p firecrab-api --all-targets`
Run: `cargo test -p firecrab-api handlers::packages:: handlers::builds:: -- --nocapture`
Expected: unchanged pass count — this step only widens visibility.

- [ ] **Step 4: Commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
git add firecrab-api/src/handlers/packages.rs firecrab-api/src/handlers/builds.rs
git commit -m "refactor: widen visibility of console-wait and builder-VM helpers for reuse"
```

---

### Task 4: `BootstrapTracker`

**Files:**
- Create: `firecrab-api/src/bootstrap.rs`
- Modify: `firecrab-api/src/main.rs` (module declaration)
- Modify: `firecrab-api/src/state.rs` (add field)

**Interfaces:**
- Consumes: `BootstrapResponse`, `BootstrapStatus` (Task 1).
- Produces: `BootstrapTracker::{begin, get, list, any_active, set_status, set_status_from, append_log, finish_ok, finish_err, finish_err_from, remove}` — mirrors `BuildTracker`'s exact shape (`firecrab-api/src/builds.rs`), keyed by a generated `bootstrap_id: Uuid`.

- [ ] **Step 1: Write the failing tests**

Create `firecrab-api/src/bootstrap.rs`:
```rust
//! In-process tracker for from-scratch distro bootstrap sessions
//! (`POST /api/images/{alias}/bootstrap` and friends) — mirrors
//! `builds::BuildTracker`'s mechanics exactly, but for a session kind whose
//! terminal action is "package as a `.tar.zst`" rather than "register a
//! template directly", so it's kept as its own tracker/type rather than
//! overloading `BuildTracker`'s `BuildStatus` with states that don't apply
//! to a customize-an-installed-template session (see
//! `docs/superpowers/specs/2026-08-03-m2image-web-rebuild-design.md`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use firecrab_api_types::{BootstrapResponse, BootstrapStatus};
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct BootstrapTracker {
    sessions: Arc<Mutex<HashMap<Uuid, BootstrapResponse>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_then_snapshot_returns_a_booting_session() {
        let tracker = BootstrapTracker::default();
        let id = tracker.begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4());

        let snapshot = tracker.get(id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Booting);
        assert_eq!(snapshot.alias, "ubuntu-26.04");
        assert_eq!(snapshot.source_alias, "alpine-3.24");
    }

    #[test]
    fn get_returns_none_for_an_unknown_id() {
        let tracker = BootstrapTracker::default();
        assert!(tracker.get(Uuid::new_v4()).is_none());
    }

    #[test]
    fn any_active_is_true_only_while_a_session_is_non_terminal() {
        let tracker = BootstrapTracker::default();
        assert!(!tracker.any_active());

        let id = tracker.begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4());
        assert!(tracker.any_active());

        tracker.finish_ok(id);
        assert!(!tracker.any_active());
    }

    #[test]
    fn set_status_from_only_applies_while_the_session_is_in_the_expected_status() {
        let tracker = BootstrapTracker::default();
        let id = tracker.begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4());

        assert!(tracker.set_status_from(id, BootstrapStatus::Booting, BootstrapStatus::Running));
        assert_eq!(tracker.get(id).unwrap().status, BootstrapStatus::Running);
        assert!(!tracker.set_status_from(id, BootstrapStatus::Booting, BootstrapStatus::Running));
    }

    #[test]
    fn finish_ok_records_succeeded_status_and_end_time() {
        let tracker = BootstrapTracker::default();
        let id = tracker.begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4());

        tracker.finish_ok(id);

        let snapshot = tracker.get(id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Succeeded);
        assert!(snapshot.ended_at_ms.is_some());
    }

    #[test]
    fn finish_err_records_failed_status_and_reason_in_the_log() {
        let tracker = BootstrapTracker::default();
        let id = tracker.begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4());

        tracker.finish_err(id, "download failed: connection reset");

        let snapshot = tracker.get(id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Failed);
        assert!(snapshot.log.contains("download failed"));
    }

    #[test]
    fn finish_err_from_only_fails_a_session_still_in_the_expected_status() {
        let tracker = BootstrapTracker::default();
        let id = tracker.begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4());

        tracker.set_status(id, BootstrapStatus::Running);
        assert!(!tracker.finish_err_from(id, BootstrapStatus::Booting, "boot timed out"));
        assert_eq!(tracker.get(id).unwrap().status, BootstrapStatus::Running);

        assert!(tracker.finish_err_from(id, BootstrapStatus::Running, "script failed"));
        assert_eq!(tracker.get(id).unwrap().status, BootstrapStatus::Failed);
    }

    #[test]
    fn remove_evicts_a_tracked_session() {
        let tracker = BootstrapTracker::default();
        let id = tracker.begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4());
        tracker.remove(id);
        assert!(tracker.get(id).is_none());
    }
}
```

Run: `cargo test -p firecrab-api bootstrap:: -- --nocapture`
Expected: FAIL (no methods implemented yet).

- [ ] **Step 2: Implement `BootstrapTracker`**

Add above the `#[cfg(test)]` module:
```rust
impl BootstrapTracker {
    /// Registers a new session in `Booting` and returns its id.
    pub fn begin(&self, alias: &str, source_alias: &str, vm_id: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        let now = now_ms();
        let session = BootstrapResponse {
            bootstrap_id: id,
            alias: alias.to_owned(),
            source_alias: source_alias.to_owned(),
            vm_id,
            status: BootstrapStatus::Booting,
            log: format!("[{}] builder VM starting", clock(now)),
            started_at_ms: now,
            ended_at_ms: None,
        };
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, session);
        id
    }

    pub fn get(&self, id: Uuid) -> Option<BootstrapResponse> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
    }

    pub fn list(&self) -> Vec<BootstrapResponse> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// Whether any tracked session hasn't reached a terminal status —
    /// `handlers::bootstrap::start_bootstrap` refuses a second session
    /// while this is true (only one bootstrap runs at a time; see the
    /// design doc's rationale — chroot/mount/mkfs on a shared build path).
    pub fn any_active(&self) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .any(|session| {
                !matches!(
                    session.status,
                    BootstrapStatus::Succeeded | BootstrapStatus::Failed
                )
            })
    }

    pub fn set_status(&self, id: Uuid, status: BootstrapStatus) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&id)
        {
            session.status = status;
        }
    }

    /// Compare-and-set: only advances a session still in `expected`,
    /// returning whether it applied — same reasoning as
    /// `BuildTracker::set_status_from` (a detached watcher must never
    /// clobber a status a later request already moved past).
    pub fn set_status_from(&self, id: Uuid, expected: BootstrapStatus, next: BootstrapStatus) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match sessions.get_mut(&id) {
            Some(session) if session.status == expected => {
                session.status = next;
                true
            }
            _ => false,
        }
    }

    pub fn append_log(&self, id: Uuid, line: impl AsRef<str>) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&id)
        {
            session.log.push('\n');
            session
                .log
                .push_str(&format!("[{}] {}", clock(now_ms()), line.as_ref()));
        }
    }

    pub fn finish_ok(&self, id: Uuid) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&id)
        {
            session.status = BootstrapStatus::Succeeded;
            session.ended_at_ms = Some(now_ms());
        }
    }

    pub fn finish_err(&self, id: Uuid, reason: impl AsRef<str>) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(session) = sessions.get_mut(&id) {
            session.status = BootstrapStatus::Failed;
            session.log.push('\n');
            session
                .log
                .push_str(&format!("[{}] {}", clock(now_ms()), reason.as_ref()));
            session.ended_at_ms = Some(now_ms());
        }
    }

    /// Compare-and-set variant of [`finish_err`](Self::finish_err) — for
    /// the same reason `BuildTracker::finish_err_from` exists.
    pub fn finish_err_from(&self, id: Uuid, expected: BootstrapStatus, reason: impl AsRef<str>) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match sessions.get_mut(&id) {
            Some(session) if session.status == expected => {
                session.status = BootstrapStatus::Failed;
                session.log.push('\n');
                session
                    .log
                    .push_str(&format!("[{}] {}", clock(now_ms()), reason.as_ref()));
                session.ended_at_ms = Some(now_ms());
                true
            }
            _ => false,
        }
    }

    /// Drops a session from the tracker (cancel path — the caller is
    /// responsible for tearing down the builder VM itself first).
    pub fn remove(&self, id: Uuid) {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn clock(epoch_ms: u64) -> String {
    format!("{}s", epoch_ms / 1000)
}
```

- [ ] **Step 3: Run tests, verify pass**

Run: `cargo test -p firecrab-api bootstrap:: -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Register the module and add to `AppState`**

In `firecrab-api/src/main.rs`, find `mod builds;` and add `mod bootstrap;` next to it (same visibility).

In `firecrab-api/src/state.rs`, add to the `AppState` struct (next to `builds: crate::builds::BuildTracker`):
```rust
    /// Async from-scratch distro bootstrap sessions (`POST /api/images/{alias}/bootstrap`).
    pub(crate) bootstraps: crate::bootstrap::BootstrapTracker,
```
Initialize it wherever `builds`/`image_installs` are initialized in `AppState::new`/`with_db_file` (same `Default::default()` pattern).

- [ ] **Step 5: Confirm the crate still builds**

Run: `cargo build -p firecrab-api`
Expected: success.

- [ ] **Step 6: Commit**

```bash
cargo fmt -p firecrab-api
git add firecrab-api/src/bootstrap.rs firecrab-api/src/state.rs firecrab-api/src/main.rs
git commit -m "feat: add BootstrapTracker for from-scratch distro bootstrap sessions"
```

---

### Task 5: `POST /api/images/{alias}/bootstrap` — boot the builder VM

**Files:**
- Create: `firecrab-api/src/handlers/bootstrap.rs`
- Modify: `firecrab-api/src/handlers/mod.rs`
- Modify: `firecrab-api/src/server.rs`

**Interfaces:**
- Consumes: `handlers::vms::{create_vm, start_vm_request}`, `handlers::builds::{builder_vm_name, builder_micro_network_id, mark_as_builder}` (Task 3), `state.bootstraps` (Task 4), `state.templates.{known_specs, list_aliases}` (existing `templates.rs`).
- Produces: `pub async fn start_bootstrap(...) -> Result<(StatusCode, Json<BootstrapResponse>), AppError>`, `pub(crate) async fn watch_bootstrap_boot(...)` — consumed by Task 7 (same file) and the frontend (Task 10).

- [ ] **Step 1: Write the failing tests**

Create `firecrab-api/src/handlers/bootstrap.rs`:
```rust
//! Web-triggered from-scratch distro bootstraps: boot a builder VM off
//! *any* already-installed template (a disposable environment, not the
//! target), run a bootstrap script over its console that downloads the
//! target's official base, chroots in, installs packages + kernel via the
//! target's own package manager, and `mkfs.ext4 -d`s a finished rootfs —
//! then dump the result out of the builder VM's disk and package it as
//! `{alias}.tar.zst` for the existing `image_install.rs` pipeline to pick
//! up unchanged. See
//! `docs/superpowers/specs/2026-08-03-m2image-web-rebuild-design.md`.

use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{BootstrapResponse, BootstrapStatus, CreateVmRequest, EgressPolicy};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::VmState;
use crate::server::RequestId;
use crate::state::AppState;
use crate::templates::TemplateRegistry;

use super::builds::{builder_micro_network_id, builder_vm_name, mark_as_builder};
use super::vms::{create_vm, parse_id, start_vm_request};

/// Matches `handlers::builds`'s own poll cadence.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Generous on purpose, same reasoning as `handlers::builds::BUILDER_BOOT_TIMEOUT`.
const BUILDER_BOOT_TIMEOUT: Duration = Duration::from_secs(600);

/// The 3 aliases this feature can bootstrap — deliberately not
/// `TemplateRegistry::known_specs()` directly, so a future built-in
/// addition doesn't silently become bootstrap-eligible without its own
/// guest script (Task 6 covers exactly these 3, no more).
const BOOTSTRAPPABLE_ALIASES: [&str; 3] = ["alpine-3.24", "ubuntu-26.04", "rocky-9"];

/// Alpine and Ubuntu bootstrap by chrooting into a freshly-downloaded base
/// that carries its own package manager, so any installed template can
/// serve as the outer builder environment. Rocky's bootstrap needs `dnf`
/// already present in the *outer* guest (see
/// `scripts/firecracker-menual/bootstrap-rocky-in-guest.sh`'s doc comment),
/// so its own builder VM must itself already be `rocky-9`.
fn requires_matching_source(target_alias: &str) -> bool {
    target_alias == "rocky-9"
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse;
    use tempfile::tempdir;

    use super::*;
    use crate::handlers::vms::test_support::test_state;

    #[tokio::test]
    async fn start_bootstrap_rejects_an_unknown_target_alias() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = start_bootstrap(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("no-such-alias".to_owned()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn start_bootstrap_rejects_when_nothing_is_installed_to_serve_as_the_builder() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

        let error = start_bootstrap(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("ubuntu-26.04".to_owned()),
        )
        .await
        .unwrap_err();

        // test_state's fixture pre-registers "ubuntu-rootfs-26.04" as an
        // installed template (see handlers::vms::test_support::test_state),
        // which IS resolvable — so bootstrapping ubuntu-26.04 (a different,
        // real known alias) from it should actually succeed at this guard.
        // This test targets alpine-3.24 instead, which the fixture never
        // installs, to exercise the "nothing eligible installed" path.
        assert_eq!(error.into_response().status(), StatusCode::UNAVAILABLE_ENTITY);
    }
}
```

`start_bootstrap` takes no request body (mirrors `start_build`'s own convention) — the target alias comes from the path, and the source (which installed template's VM does the work) is picked automatically, not chosen by the caller.

Run: `cargo test -p firecrab-api handlers::bootstrap:: -- --nocapture`
Expected: FAIL (`start_bootstrap` not defined).

**Before Step 2:** the second test above references `StatusCode::UNAVAILABLE_ENTITY`, which does not exist in `axum::http::StatusCode` — this is deliberately wrong so the test fails to compile at this stage; fix it in Step 2 by using whichever status `AppError::unavailable` actually maps to (check `firecrab-api/src/error.rs`, confirmed elsewhere in this codebase to be `503 SERVICE_UNAVAILABLE` — replace with `StatusCode::SERVICE_UNAVAILABLE`).

- [ ] **Step 2: Implement `start_bootstrap` and `watch_bootstrap_boot`**

Fix the test's `StatusCode::UNAVAILABLE_ENTITY` to `StatusCode::SERVICE_UNAVAILABLE`. Then add, above the test module:
```rust
/// `POST /api/images/{alias}/bootstrap` — boots a builder VM off any
/// already-installed template and registers a new bootstrap session for
/// `alias`. Returns immediately, same convention as `start_build`; the
/// caller polls `GET /api/images/bootstrap/{bootstrapId}`.
pub async fn start_bootstrap(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(alias): Path<String>,
) -> Result<(StatusCode, Json<BootstrapResponse>), AppError> {
    if !BOOTSTRAPPABLE_ALIASES.contains(&alias.as_str()) {
        return Err(AppError::not_found(request_id.0));
    }

    if state.bootstraps.any_active() {
        return Err(AppError::conflict(
            "bootstrap_in_progress",
            "a bootstrap is already running; wait for it to finish before starting another",
            request_id.0,
        ));
    }

    let source_alias = pick_builder_source(&state, &alias, request_id.0)?;
    let source = state
        .templates
        .resolve_alias(&source_alias)
        .ok_or_else(|| AppError::internal(request_id.0))?;

    let micro_network_id = builder_micro_network_id(&state, request_id.0).await?;

    let create_request = CreateVmRequest {
        name: builder_vm_name(&format!("bootstrap-{alias}")),
        template: source_alias.clone(),
        ram: 1024,
        cpu: 1,
        disk_gb: bootstrap_disk_gb(&alias),
        egress_policy: EgressPolicy::Internet,
        micro_network_id,
        storage_root: None,
    };
    let _ = source; // only needed to confirm the source alias actually resolves

    let (_status, Json(created)) = create_vm(
        State(state.clone()),
        Extension(request_id),
        ValidatedJson(create_request),
    )
    .await?;

    mark_as_builder(&state, created.id, request_id.0).await?;

    let _vm_response = start_vm_request(
        State(state.clone()),
        Extension(request_id),
        Path(created.id.to_string()),
    )
    .await?;

    let bootstrap_id = state.bootstraps.begin(&alias, &source_alias, created.id);

    let state_for_watch = state.clone();
    tokio::spawn(async move {
        watch_bootstrap_boot(&state_for_watch, bootstrap_id, created.id).await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(state.bootstraps.get(bootstrap_id).expect("just inserted")),
    ))
}

/// Every installed rootfs's disk floor plus generous headroom for: the
/// downloaded official base archive, the fully-installed staging tree, and
/// the final `mkfs.ext4`'d image sitting alongside it before it's dumped
/// out — all three exist on the builder VM's own disk at once mid-build.
/// Sized per target rather than derived from the source template, since the
/// source is just a disposable outer environment unrelated to how big the
/// target ends up.
fn bootstrap_disk_gb(target_alias: &str) -> u16 {
    match target_alias {
        "alpine-3.24" => 4,
        _ => 8, // ubuntu-26.04, rocky-9 — 2G rootfs_size each, per default_specs()
    }
}

/// Picks an already-installed template to boot as the builder VM.
/// `requires_matching_source` narrows this to the target itself for
/// aliases whose bootstrap needs the outer guest to already have that
/// distro's own package manager (currently just `rocky-9`, see its own
/// doc comment) — everything else accepts any installed alias, preferring
/// the smallest rootfs since it boots fastest.
fn pick_builder_source(state: &AppState, target_alias: &str, request_id: Uuid) -> Result<String, AppError> {
    let candidates = state.templates.list_aliases();
    let mut eligible: Vec<_> = candidates
        .into_iter()
        .filter(|version| !requires_matching_source(target_alias) || version.name == target_alias)
        .collect();
    eligible.sort_by_key(|version| version.rootfs.length());

    eligible
        .into_iter()
        .next()
        .map(|version| version.name.clone())
        .ok_or_else(|| {
            AppError::unavailable(
                if requires_matching_source(target_alias) {
                    "bootstrapping rocky-9 needs rocky-9 already installed to provide dnf — install it first"
                } else {
                    "no template is installed yet to serve as the builder VM — install one first"
                },
                request_id,
            )
        })
}

/// Polls the builder VM's lifecycle state until it reaches `Running`
/// (session becomes `Running`... — see note below), a terminal failure, or
/// [`BUILDER_BOOT_TIMEOUT`] elapses. Mirrors
/// `handlers::builds::watch_builder_boot` exactly (same CAS-against-`Booting`
/// safety reasoning), adapted to `BootstrapStatus`'s own states — note this
/// module's `Running` means "VM up, bootstrap script executing", not
/// `BuildStatus::Ready`'s "VM up, waiting for a command" — because a
/// bootstrap session has no separate `Ready`-then-command step: the whole
/// script is dispatched as soon as the VM is usable (Task 7).
pub(crate) async fn watch_bootstrap_boot(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let deadline = tokio::time::Instant::now() + BUILDER_BOOT_TIMEOUT;
    loop {
        match state.bootstraps.get(bootstrap_id) {
            Some(session) if session.status == BootstrapStatus::Booting => {}
            _ => return,
        }

        let vm_state = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&vm_id)
            .map(|vm| vm.state);

        match vm_state {
            Some(VmState::Running) => {
                state
                    .bootstraps
                    .append_log(bootstrap_id, "builder VM is running — starting bootstrap script");
                state
                    .bootstraps
                    .set_status_from(bootstrap_id, BootstrapStatus::Booting, BootstrapStatus::Running);
                return;
            }
            Some(state_now @ (VmState::Error | VmState::Stopped)) => {
                state.bootstraps.finish_err_from(
                    bootstrap_id,
                    BootstrapStatus::Booting,
                    format!("builder VM {vm_id} failed to boot (state: {state_now:?})"),
                );
                return;
            }
            None => return,
            Some(_) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            state.bootstraps.finish_err_from(
                bootstrap_id,
                BootstrapStatus::Booting,
                format!("builder VM {vm_id} did not reach running within {}s", BUILDER_BOOT_TIMEOUT.as_secs()),
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
```

Note: `watch_bootstrap_boot` sets the session straight to `Running` on boot (not a separate "ready" state) — Task 7's `run_bootstrap_script` is dispatched from right here in a follow-up (Task 7 modifies this function to also kick off the script once `Running` is set, rather than leaving the session sitting there with nothing watching it — see Task 7 Step 2 for the exact edit). For now, this task only needs the boot-watching half to compile and pass its own tests; the "nothing ever leaves `Running`" gap until Task 7 lands is expected and matches how `handlers::builds` was built incrementally in the prior plan.

- [ ] **Step 3: Run tests, verify pass**

Run: `cargo test -p firecrab-api handlers::bootstrap:: -- --nocapture`
Expected: PASS. (The first test needs `pick_builder_source`'s not-found path to hit before the "nothing installed" path — since `test_state`'s fixture always has `ubuntu-rootfs-26.04` installed, requesting bootstrap of `alpine-3.24` — which the fixture never installs and isn't `rocky-9` so doesn't require a matching source — still finds `ubuntu-rootfs-26.04` as an eligible source and would NOT hit the "unavailable" branch. Fix the test to target `rocky-9` instead, which DOES require a matching source the fixture never provides:
```rust
    #[tokio::test]
    async fn start_bootstrap_rejects_when_no_matching_source_is_installed() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

        let error = start_bootstrap(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("rocky-9".to_owned()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::SERVICE_UNAVAILABLE);
    }
```
Replace the earlier draft of this test with this corrected version before running.)

- [ ] **Step 4: Register the module and wire the route**

In `firecrab-api/src/handlers/mod.rs`, add `pub mod bootstrap;` alongside `pub mod builds;`.

In `firecrab-api/src/server.rs`, add near the `/api/images/{alias}/build` route:
```rust
        .route(
            "/api/images/{alias}/bootstrap",
            post(handlers::bootstrap::start_bootstrap),
        )
```

- [ ] **Step 5: Full check + commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
git add firecrab-api/src/handlers/bootstrap.rs firecrab-api/src/handlers/mod.rs firecrab-api/src/server.rs
git commit -m "feat: add POST /api/images/{alias}/bootstrap to boot the builder VM"
```

---

### Task 6: Guest-side bootstrap scripts (Alpine, Ubuntu, Rocky)

**Files:**
- Create: `scripts/firecracker-menual/bootstrap-alpine-in-guest.sh`
- Create: `scripts/firecracker-menual/bootstrap-ubuntu-in-guest.sh`
- Create: `scripts/firecracker-menual/bootstrap-rocky-in-guest.sh`

**Interfaces:**
- Consumes: nothing from Rust — these are POSIX shell scripts that run entirely inside the builder guest, pushed over the console by Task 7.
- Produces: on success, each script leaves exactly these files under `/root/fc-bootstrap/out/` inside the guest:
  - `rootfs.ext4` — the finished target rootfs, sized to that target's `rootfs_size`
  - `vmlinux` — the extracted ELF kernel
  - `initramfs` — only for alpine and rocky (both need one; ubuntu does not — omit this file for ubuntu)

These generic names (not the final `vmlinux-alpine-virt-x86_64`-style filenames) are deliberate: Task 8's packaging step renames them to the exact names `templates.rs::default_specs()` expects when it builds the `.tar.zst`, so the three scripts don't need to duplicate that naming knowledge.

All three follow the same shape, adapted from the existing `install-{alpine,ubuntu,rocky}-rootfs.sh` (their host/docker versions) with the docker-container or host-`sudo` wrapper removed — the guest is already root, so nothing here elevates privileges; it's already running as them.

- [ ] **Step 1: Create `bootstrap-alpine-in-guest.sh`**

Adapted from `install-alpine-rootfs.sh`'s `write_configure_script` body: instead of the outer `alpine:latest` docker container running `apk --root "$staging"` with ITS OWN `apk`, this chroots into the freshly-extracted minirootfs and uses THAT minirootfs's own bundled `/sbin/apk` — every Alpine minirootfs ships one, so this needs nothing Alpine-specific pre-installed in the outer builder guest, only generic tools (`curl`, `tar`, `chroot`, `mount`, `mkfs.ext4`).

```sh
#!/bin/sh
# Runs entirely inside a firecrab builder VM (any installed template) —
# downloads the official Alpine minirootfs, chroots in and installs
# packages/kernel via ITS OWN bundled apk (not the outer guest's), then
# packs the result into an ext4 image via `mkfs.ext4 -d` (no loop mount).
# Adapted from install-alpine-rootfs.sh's write_configure_script — same
# package list/config files, minus the outer-docker-container wrapper.
set -eu

work=/root/fc-bootstrap
staging="$work/staging"
out="$work/out"
alpine_releases_base='https://dl-cdn.alpinelinux.org/alpine'
rootfs_size='512M'
rootfs_hostname='firecrab'
rootfs_packages='alpine-baselayout busybox openrc agetty iproute2-minimal iputils-ping dhcpcd openssh-server ca-certificates curl procps linux-virt'

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

cleanup_mounts() {
  umount -R "$staging/proc" 2>/dev/null || true
  umount -R "$staging/sys" 2>/dev/null || true
  umount -R "$staging/dev" 2>/dev/null || true
}
trap cleanup_mounts EXIT

rm -rf "$work"
mkdir -p "$staging" "$out"

arch=$(uname -m)
case "$arch" in
  x86_64) ;;
  *) fail "unsupported architecture: $arch" ;;
esac

info 'resolving latest Alpine 3.24 minirootfs release'
releases_yaml="$work/latest-releases.yaml"
curl -fsSL "${alpine_releases_base}/v3.24/releases/${arch}/latest-releases.yaml" -o "$releases_yaml" \
  || fail 'could not download Alpine release metadata'

set -- $(awk '
  function emit() { if (flavor == "alpine-minirootfs") { printf "%s %s %s %s", branch, version, file, sha256; found = 1 } }
  /^-[[:space:]]*$/ { emit(); if (found) exit; branch=""; version=""; file=""; sha256=""; flavor=""; next }
  /^  branch:/ { branch = $2 }
  /^  version:/ { version = $2 }
  /^  flavor:/ { flavor = $2 }
  /^  file:/ { file = $2 }
  /^  sha256:/ { sha256 = $2 }
  END { if (!found) emit() }
' "$releases_yaml")
branch=$1
version=$2
archive_file=$3
archive_sha256=$4
[ -n "$branch" ] && [ -n "$archive_file" ] || fail 'could not resolve the Alpine minirootfs release'
info "Alpine branch ${branch}, minirootfs version ${version}"

archive_path="$work/${archive_file}"
curl -fsSL "${alpine_releases_base}/${branch}/releases/${arch}/${archive_file}" -o "$archive_path" \
  || fail 'could not download the Alpine minirootfs archive'
printf '%s  %s\n' "$archive_sha256" "$archive_path" | sha256sum -c - || fail 'minirootfs checksum mismatch'

info 'extracting minirootfs'
tar -xzf "$archive_path" -C "$staging"

cat >"${staging}/etc/apk/repositories" <<REPOS
${alpine_releases_base}/${branch}/main
${alpine_releases_base}/${branch}/community
REPOS

mount -t proc proc "$staging/proc"
mount --rbind /sys "$staging/sys"
mount --rbind /dev "$staging/dev"

info "installing packages: ${rootfs_packages}"
chroot "$staging" /sbin/apk add --no-cache --update-cache $rootfs_packages
chroot "$staging" /sbin/apk add --no-cache e2fsprogs

cat >"${staging}/etc/hostname" <<EOF
${rootfs_hostname}
EOF
cat >"${staging}/etc/hosts" <<EOF
127.0.0.1 localhost
127.0.1.1 ${rootfs_hostname}
EOF
cat >"${staging}/etc/fstab" <<'EOF'
/dev/vda / ext4 defaults 0 1
EOF
cat >"${staging}/etc/resolv.conf" <<'EOF'
nameserver 172.30.0.1
EOF
mkdir -p "${staging}/etc/network"
cat >"${staging}/etc/network/interfaces" <<'EOF'
auto lo
iface lo inet loopback

auto eth0
iface eth0 inet dhcp
EOF

cat >"${staging}/etc/init.d/firecrab-network-ready" <<'EOF'
#!/sbin/openrc-run
description="Firecrab network readiness sentinel"
depend() {
    need net
    after dhcpcd
}
start() {
    ipv4=""
    for _ in $(seq 1 10); do
        ipv4=$(ip -4 -o addr show eth0 2>/dev/null | awk '{print $4}' | cut -d/ -f1)
        [ -n "$ipv4" ] && break
        sleep 1
    done
    if [ -z "$ipv4" ]; then
        echo "FIRECRAB_NETWORK_FAILED no-ipv4-address" >/dev/console
    elif getent hosts example.com >/dev/null 2>&1; then
        echo "FIRECRAB_NETWORK_READY $ipv4" >/dev/console
    else
        echo "FIRECRAB_NETWORK_FAILED dns-unreachable" >/dev/console
    fi
}
EOF
chmod 0755 "${staging}/etc/init.d/firecrab-network-ready"

grep -v '^ttyS0::' "${staging}/etc/inittab" >"${staging}/etc/inittab.new"
printf 'ttyS0::respawn:/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 ttyS0 vt100\n' \
  >>"${staging}/etc/inittab.new"
mv "${staging}/etc/inittab.new" "${staging}/etc/inittab"

mkdir -p "${staging}/etc/runlevels/sysinit" "${staging}/etc/runlevels/boot" "${staging}/etc/runlevels/default"
for svc in devfs dmesg; do ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/sysinit/${svc}"; done
for svc in hostname bootmisc sysctl loopback; do ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/boot/${svc}"; done
for svc in local dhcpcd sshd firecrab-network-ready; do ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/default/${svc}"; done

test -e "${staging}/boot/vmlinuz-virt" || fail 'missing boot/vmlinuz-virt (linux-virt)'
test -e "${staging}/boot/initramfs-virt" || fail 'missing boot/initramfs-virt (linux-virt)'
cp "${staging}/boot/vmlinuz-virt" "$out/vmlinuz-virt-raw"
cp "${staging}/boot/initramfs-virt" "$out/initramfs"

cleanup_mounts

info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$staging" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# extract-vmlinux ships alongside this script in the repo but does not
# exist inside the guest — the raw vmlinuz is dumped out as-is
# ($out/vmlinuz-virt-raw) and Task 8 runs extract-vmlinux on the HOST
# after pulling it out of the guest disk, since the host is already known
# to have every decompressor it might need (same reasoning
# install-alpine-rootfs.sh's own extract_kernel already used).
info 'bootstrap complete'
```

- [ ] **Step 2: Create `bootstrap-ubuntu-in-guest.sh`**

Adapted from `install-ubuntu-roofs.sh`: removes the `[ "$(id -u)" -ne 0 ] && exec sudo ...` re-exec (the guest is already root) and the host-ownership-restoring step (`restore_output_ownership`, meaningless inside a disposable guest). Keeps the chroot+mount+`apt-get`+`mkfs.ext4 -d` core exactly as-is, since that part already needed no docker.

```sh
#!/bin/sh
# Runs entirely inside a firecrab builder VM (any installed template) —
# downloads the official Ubuntu Base tarball, chroots in and installs
# packages/kernel via ITS OWN apt (not the outer guest's), then packs the
# result into an ext4 image via `mkfs.ext4 -d` (no loop mount). Adapted
# from install-ubuntu-roofs.sh, minus its host-sudo re-exec and
# ownership-restore steps — the guest is already root and disposable.
set -eu

work=/root/fc-bootstrap
mount_dir="$work/staging"
out="$work/out"
ubuntu_base_url='https://cdimage.ubuntu.com/ubuntu-base/releases'
series='26.04'
rootfs_size='2G'
rootfs_hostname='firecrab'
rootfs_packages='systemd systemd-sysv udev kmod util-linux linux-image-generic iproute2 iputils-ping net-tools dnsutils curl ca-certificates procps openssh-server'

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

cleanup_mounts() {
  umount -R "$mount_dir/proc" 2>/dev/null || true
  umount -R "$mount_dir/sys" 2>/dev/null || true
  umount -R "$mount_dir/dev" 2>/dev/null || true
}
trap cleanup_mounts EXIT

rm -rf "$work"
mkdir -p "$mount_dir" "$out"

arch=amd64
case "$(uname -m)" in
  x86_64) arch=amd64 ;;
  *) fail "unsupported architecture: $(uname -m)" ;;
esac

release_url="${ubuntu_base_url}/${series}/release"
index_html="$work/index.html"
curl -fsSL "${release_url}/" -o "$index_html" || fail 'could not download Ubuntu Base release index'
archive_name=$(grep -Eo "ubuntu-base-[0-9.]+-base-${arch}\\.tar\\.gz" "$index_html" | sort -V | tail -n1)
[ -n "$archive_name" ] || fail "could not find an Ubuntu Base archive for ${series}/${arch}"

archive_path="$work/${archive_name}"
curl -fsSL "${release_url}/${archive_name}" -o "$archive_path" || fail 'could not download the Ubuntu Base archive'

checksum_file="$work/SHA256SUMS"
curl -fsSL "${release_url}/SHA256SUMS" -o "$checksum_file" || fail 'could not download Ubuntu Base checksums'
checksum_line=$(grep -E "[ *]${archive_name}\$" "$checksum_file") || fail 'checksum entry not found'
(cd "$work" && printf '%s\n' "$checksum_line" | sha256sum -c -) || fail 'Ubuntu Base checksum mismatch'

info 'extracting Ubuntu Base'
tar --numeric-owner -xpf "$archive_path" -C "$mount_dir"

cat >"${mount_dir}/etc/hostname" <<EOF
${rootfs_hostname}
EOF
cat >"${mount_dir}/etc/hosts" <<EOF
127.0.0.1 localhost
127.0.1.1 ${rootfs_hostname}
EOF
cat >"${mount_dir}/etc/fstab" <<'EOF'
/dev/vda / ext4 defaults 0 1
EOF
: >"${mount_dir}/etc/machine-id"

install -d -m 0755 "${mount_dir}/etc/systemd/network"
cat >"${mount_dir}/etc/systemd/network/10-eth0.network" <<'EOF'
[Match]
Name=eth0

[Network]
DHCP=yes
EOF
install -d -m 0755 "${mount_dir}/etc/systemd/system/multi-user.target.wants"
ln -sf /lib/systemd/system/systemd-networkd.service \
  "${mount_dir}/etc/systemd/system/multi-user.target.wants/systemd-networkd.service"
install -d -m 0755 "${mount_dir}/etc/systemd/system/sockets.target.wants"
ln -sf /lib/systemd/system/systemd-networkd.socket \
  "${mount_dir}/etc/systemd/system/sockets.target.wants/systemd-networkd.socket"

install -d -m 0755 "${mount_dir}/usr/local/sbin"
cat >"${mount_dir}/usr/local/sbin/firecrab-network-ready.sh" <<'EOF'
#!/bin/sh
set -eu
ipv4=""
for _ in $(seq 1 10); do
  ipv4=$(ip -4 -o addr show eth0 2>/dev/null | awk '{print $4}' | cut -d/ -f1)
  [ -n "$ipv4" ] && break
  sleep 1
done
if [ -z "$ipv4" ]; then
  echo "FIRECRAB_NETWORK_FAILED no-ipv4-address"
elif getent hosts example.com >/dev/null 2>&1; then
  echo "FIRECRAB_NETWORK_READY $ipv4"
else
  echo "FIRECRAB_NETWORK_FAILED dns-unreachable"
fi
EOF
chmod 0755 "${mount_dir}/usr/local/sbin/firecrab-network-ready.sh"
cat >"${mount_dir}/etc/systemd/system/firecrab-network-ready.service" <<'EOF'
[Unit]
Description=Firecrab network readiness sentinel
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
StandardOutput=tty
TTYPath=/dev/console
ExecStart=/usr/local/sbin/firecrab-network-ready.sh

[Install]
WantedBy=multi-user.target
EOF
install -d -m 0755 "${mount_dir}/etc/systemd/system/multi-user.target.wants"
ln -sf /etc/systemd/system/firecrab-network-ready.service \
  "${mount_dir}/etc/systemd/system/multi-user.target.wants/firecrab-network-ready.service"

install -d -m 0755 "${mount_dir}/etc/systemd/system/getty.target.wants"
ln -sf /lib/systemd/system/serial-getty@.service \
  "${mount_dir}/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service"
install -d -m 0755 "${mount_dir}/etc/systemd/system/serial-getty@ttyS0.service.d"
cat >"${mount_dir}/etc/systemd/system/serial-getty@ttyS0.service.d/autologin.conf" <<'EOF'
[Unit]
BindsTo=
After=
After=systemd-user-sessions.service getty-pre.target

[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 %I $TERM
EOF

install -d -m 0755 "${mount_dir}/dev" "${mount_dir}/proc" "${mount_dir}/sys" "${mount_dir}/run"
install -d -m 1777 "${mount_dir}/tmp"

cp /etc/resolv.conf "${mount_dir}/etc/resolv.conf"
mount -t proc proc "${mount_dir}/proc"
mount --rbind /sys "${mount_dir}/sys"
mount --rbind /dev "${mount_dir}/dev"

info "installing packages: ${rootfs_packages}"
chroot "$mount_dir" env DEBIAN_FRONTEND=noninteractive apt-get update
chroot "$mount_dir" env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends $rootfs_packages
chroot "$mount_dir" apt-get clean
rm -rf "${mount_dir}/var/lib/apt/lists/"*

vmlinuz_path=$(find "${mount_dir}/boot" -maxdepth 1 -name 'vmlinuz-*' | sort -V | tail -n1)
[ -n "$vmlinuz_path" ] || fail 'linux-image-generic did not install a vmlinuz'
cp "$vmlinuz_path" "$out/vmlinuz-raw"

cat >"${mount_dir}/etc/resolv.conf" <<'EOF'
nameserver 172.30.0.1
EOF

cleanup_mounts

test -e "${mount_dir}/etc/os-release" || fail 'missing /etc/os-release'
test -e "${mount_dir}/sbin/init" || fail 'missing /sbin/init'
test -e "${mount_dir}/usr/sbin/sshd" || fail 'missing sshd'

info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$mount_dir" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# Same reasoning as the Alpine script: extract-vmlinux runs on the HOST
# (Task 8) against $out/vmlinuz-raw once it's been dumped out of this
# guest's own disk — not in here.
info 'bootstrap complete'
```

- [ ] **Step 3: Create `bootstrap-rocky-in-guest.sh`**

Rocky's own package acquisition (`dnf --installroot=...`) needs `dnf`/`rpm` already present in the *outer* guest — unlike Alpine/Ubuntu, there is no minimal "Rocky base tarball with its own bundled dnf" this can chroot into first. `start_bootstrap` (Task 5) already enforces this by requiring the builder VM's source to be `rocky-9` itself (`requires_matching_source`), and since the rocky-9 template's own `rootfs_packages` already includes `e2fsprogs`/`dnf` (they're part of what makes a running rocky-9 instance a rocky-9 instance), the outer guest needs no prerequisite installation before this script runs — unlike the original script's throwaway container, which installed `e2fsprogs` into itself first.

Adapted from `install-rocky-rootfs.sh`'s `write_configure_script` body: same `dnf --installroot` transaction, same `dracut`-based generic initramfs generation (both already ran inside a chroot/mount context that needed no docker privilege beyond `mount`/`chroot` themselves — the outer `docker run --cap-add=SYS_ADMIN ...` wrapper existed only to grant a normally-unprivileged container that capability; a real microVM guest already has it natively), same NetworkManager-based network config (Rocky's template uses NetworkManager, not systemd-networkd — keep that as-is, don't copy Ubuntu's approach here), same PCI-transport kernel-config guard, same DHCP profile without a pinned interface name (`ip -4 -o addr show scope global` in the readiness sentinel, not `eth0`-specific, since Rocky may rename its interface under PCI transport).

```sh
#!/bin/sh
# Runs entirely inside a firecrab builder VM that is itself already
# rocky-9 (enforced by handlers::bootstrap::requires_matching_source,
# since this needs dnf already present in the outer guest — unlike
# Alpine/Ubuntu, Rocky has no minimal base tarball with its own bundled
# package manager to chroot into first). Adapted from
# install-rocky-rootfs.sh's write_configure_script, minus the outer
# `docker run --cap-add=SYS_ADMIN ...` wrapper — a real microVM guest
# already has mount/chroot natively, no capability grant needed.
set -eu

work=/root/fc-bootstrap
staging="$work/staging"
out="$work/out"
rootfs_size='2G'
rootfs_hostname='firecrab'
baseos_url='https://download.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os/'
appstream_url='https://download.rockylinux.org/pub/rocky/9/AppStream/x86_64/os/'
rootfs_packages='kernel dracut systemd systemd-udev NetworkManager iproute iputils bind-utils curl ca-certificates procps-ng openssh-server kmod util-linux dhcp-client e2fsprogs'

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

chroot_mounts=''
cleanup_chroot_mounts() {
  for target in $chroot_mounts; do
    umount -R "$target" 2>/dev/null || umount -l "$target" 2>/dev/null || true
  done
  chroot_mounts=''
}
trap cleanup_chroot_mounts EXIT

mount_chroot_fs() {
  mount -t proc proc "$staging/proc"
  chroot_mounts="$staging/proc"
  mount --rbind /sys "$staging/sys"
  mount --make-rslave "$staging/sys"
  chroot_mounts="$staging/sys $chroot_mounts"
  mount --rbind /dev "$staging/dev"
  mount --make-rslave "$staging/dev"
  chroot_mounts="$staging/dev $chroot_mounts"
  mount --rbind /run "$staging/run"
  mount --make-rslave "$staging/run"
  chroot_mounts="$staging/run $chroot_mounts"
}

rm -rf "$work"
mkdir -p "$staging/etc/pki" "$staging/dev" "$staging/proc" "$staging/sys" "$staging/run" "$out"
cp -a /etc/pki/rpm-gpg "$staging/etc/pki/"

dnf_common="--disablerepo=* --enablerepo=baseos,appstream --setopt=baseos.mirrorlist= --setopt=baseos.baseurl=${baseos_url} --setopt=appstream.mirrorlist= --setopt=appstream.baseurl=${appstream_url} --setopt=install_weak_deps=False --setopt=keepcache=False"

info 'installing Rocky Linux 9 guest packages into the staging root'
mount_chroot_fs
# shellcheck disable=SC2086 -- package/flag lists are deliberate whitespace lists.
dnf -q -y --installroot="$staging" --releasever=9 --setopt=reposdir=/etc/yum.repos.d \
  $dnf_common install $rootfs_packages

rm -rf "$staging/var/cache/dnf" "$staging/var/log/dnf"* "$staging/var/cache/yum" "$staging/var/log/yum"* 2>/dev/null || true

cat >"$staging/etc/hostname" <<EOF
${rootfs_hostname}
EOF
cat >"$staging/etc/hosts" <<EOF
127.0.0.1 localhost
127.0.1.1 ${rootfs_hostname}
EOF
cat >"$staging/etc/fstab" <<'EOF'
/dev/vda / ext4 defaults 0 1
EOF
: >"$staging/etc/machine-id"
rm -f "$staging/etc/resolv.conf"
cat >"$staging/etc/resolv.conf" <<'EOF'
nameserver 172.30.0.1
EOF

install -d -m 0755 "$staging/etc/NetworkManager/system-connections"
cat >"$staging/etc/NetworkManager/system-connections/firecrab-ethernet.nmconnection" <<'EOF'
[connection]
id=firecrab-ethernet
type=ethernet
autoconnect=true

[ipv4]
method=auto
may-fail=false

[ipv6]
method=disabled
EOF
chmod 0600 "$staging/etc/NetworkManager/system-connections/firecrab-ethernet.nmconnection"

install -d -m 0755 \
  "$staging/etc/systemd/system/multi-user.target.wants" \
  "$staging/etc/systemd/system/network-online.target.wants" \
  "$staging/etc/systemd/system/getty.target.wants" \
  "$staging/etc/systemd/system/serial-getty@ttyS0.service.d"
ln -sfn /usr/lib/systemd/system/NetworkManager.service \
  "$staging/etc/systemd/system/multi-user.target.wants/NetworkManager.service"
ln -sfn /usr/lib/systemd/system/NetworkManager-wait-online.service \
  "$staging/etc/systemd/system/network-online.target.wants/NetworkManager-wait-online.service"
ln -sfn /usr/lib/systemd/system/serial-getty@.service \
  "$staging/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service"
ln -sfn /usr/lib/systemd/system/sshd.service \
  "$staging/etc/systemd/system/multi-user.target.wants/sshd.service"

cat >"$staging/etc/systemd/system/serial-getty@ttyS0.service.d/autologin.conf" <<'EOF'
[Unit]
BindsTo=
After=
After=systemd-user-sessions.service getty-pre.target

[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 %I $TERM
EOF

install -d -m 0755 "$staging/usr/local/sbin"
cat >"$staging/usr/local/sbin/firecrab-network-ready.sh" <<'EOF'
#!/bin/sh
set -eu
ipv4=""
for _ in $(seq 1 15); do
    ipv4=$(ip -4 -o addr show scope global 2>/dev/null | \
        awk '$2 != "lo" { split($4, address, "/"); print address[1]; exit }')
    [ -n "$ipv4" ] && break
    sleep 1
done
if [ -z "$ipv4" ]; then
    echo "FIRECRAB_NETWORK_FAILED no-ipv4-address"
elif getent hosts example.com >/dev/null 2>&1; then
    echo "FIRECRAB_NETWORK_READY $ipv4"
else
    echo "FIRECRAB_NETWORK_FAILED dns-unreachable"
fi
EOF
chmod 0755 "$staging/usr/local/sbin/firecrab-network-ready.sh"

cat >"$staging/etc/systemd/system/firecrab-network-ready.service" <<'EOF'
[Unit]
Description=Firecrab network readiness sentinel
After=NetworkManager-wait-online.service
Wants=NetworkManager-wait-online.service

[Service]
Type=oneshot
StandardOutput=tty
TTYPath=/dev/console
ExecStart=/usr/local/sbin/firecrab-network-ready.sh

[Install]
WantedBy=multi-user.target
EOF
ln -sfn /etc/systemd/system/firecrab-network-ready.service \
  "$staging/etc/systemd/system/multi-user.target.wants/firecrab-network-ready.service"

# EL9's kernel-install layout keeps the raw kernel under
# /usr/lib/modules/<version>/vmlinuz (no separate /boot/vmlinuz-* copy).
vmlinuz_path=$(find "$staging/usr/lib/modules" -mindepth 2 -maxdepth 2 -type f -name vmlinuz | sort -V | tail -n1)
[ -n "$vmlinuz_path" ] || fail 'Rocky kernel package did not install usr/lib/modules/*/vmlinuz'
kernel_version=$(basename "$(dirname "$vmlinuz_path")")
initrd_path="$staging/boot/initramfs-${kernel_version}.img"
kernel_config="$staging/usr/lib/modules/${kernel_version}/config"

grep -Eq '^CONFIG_VIRTIO_PCI=(y|m)$' "$kernel_config" || fail "Rocky kernel lacks CONFIG_VIRTIO_PCI: ${kernel_config}"

info "building generic dracut initramfs for ${kernel_version}"
chroot "$staging" /usr/bin/dracut --force --no-hostonly \
  --add-drivers 'virtio_blk virtio_pci virtio_net ext4' \
  "/boot/initramfs-${kernel_version}.img" "$kernel_version"
cleanup_chroot_mounts

[ -s "$initrd_path" ] || fail "dracut did not create ${initrd_path}"
test -e "$staging/etc/os-release" || fail 'missing /etc/os-release'
test -e "$staging/sbin/init" || fail 'missing /sbin/init'
test -x "$staging/usr/sbin/sshd" || fail 'missing sshd'

cp "$vmlinuz_path" "$out/vmlinuz-raw"
cp "$initrd_path" "$out/initramfs"

info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$staging" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# Same reasoning as the Alpine/Ubuntu scripts: extract-vmlinux runs on the
# HOST (Task 8) against $out/vmlinuz-raw once it's dumped out of this
# guest's own disk.
info 'bootstrap complete'
```

Self-review this against the real `install-rocky-rootfs.sh` line by line before moving on — in particular confirm the `dnf_common` flags, the `dracut --add-drivers` argument, and the `CONFIG_VIRTIO_PCI` guest-config check were all carried over unchanged, since dropping any of them would produce a rootfs that boots on the original CLI path but not this one.

- [ ] **Step 4: Make all three executable and commit**

```bash
chmod +x scripts/firecracker-menual/bootstrap-alpine-in-guest.sh \
  scripts/firecracker-menual/bootstrap-ubuntu-in-guest.sh \
  scripts/firecracker-menual/bootstrap-rocky-in-guest.sh
git add scripts/firecracker-menual/bootstrap-alpine-in-guest.sh \
  scripts/firecracker-menual/bootstrap-ubuntu-in-guest.sh \
  scripts/firecracker-menual/bootstrap-rocky-in-guest.sh
git commit -m "feat: add guest-native bootstrap scripts for alpine/ubuntu/rocky"
```

No automated test covers these three scripts directly (they need a real network + a real builder VM) — Task 7's implementer runs each one manually inside an actual booted builder VM as part of that task's own verification, which is the first real end-to-end proof any of this works. Flag any script bug found there as a fix to the specific script, not a plan defect.

---

### Task 7: Push the bootstrap script over the console and wait for it to finish

**Files:**
- Modify: `firecrab-api/src/handlers/bootstrap.rs`
- Modify: `firecrab-api/src/handlers/builds.rs` (one small edit — see Step 1)

**Interfaces:**
- Consumes: `packages::{wait_for_completion, find_done_sentinel, OUTPUT_TAIL_CAP}` (Task 3), `state.processes` (existing `VmProcess`/`ConsoleBroker`).
- Produces: `pub(crate) async fn run_bootstrap_script(...)`, wired to fire automatically once `watch_bootstrap_boot` (Task 5) reaches `Running` — no separate HTTP call needed, unlike `build_packages`, since a bootstrap session has exactly one script to run, not an open-ended sequence of package actions the operator chooses.

- [ ] **Step 1: Wire `run_bootstrap_script` to fire when the VM becomes usable**

In `watch_bootstrap_boot` (Task 5, `handlers/bootstrap.rs`), replace the `Some(VmState::Running) => { ... return; }` arm's body so it spawns the script runner instead of just marking `Running` and returning:
```rust
            Some(VmState::Running) => {
                state
                    .bootstraps
                    .append_log(bootstrap_id, "builder VM is running — starting bootstrap script");
                if state
                    .bootstraps
                    .set_status_from(bootstrap_id, BootstrapStatus::Booting, BootstrapStatus::Running)
                {
                    let state_for_script = state.clone();
                    tokio::spawn(async move {
                        run_bootstrap_script(&state_for_script, bootstrap_id, vm_id).await;
                    });
                }
                return;
            }
```
(The `if` around the `tokio::spawn` matters: `set_status_from` can return `false` if something else — a cancel — already moved the session off `Booting` between the match arm being entered and this line; in that case, nothing should be spawned against a session that's no longer live.)

- [ ] **Step 2: Write the failing test**

Add to `handlers/bootstrap.rs`'s test module:
```rust
    #[tokio::test]
    async fn run_bootstrap_script_records_the_console_output_and_reaches_running_terminal_wait() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = crate::handlers::vms::test_support::record("builder", Uuid::new_v4());
        crate::handlers::vms::test_support::seed_vm(&state, &vm);
        let console = register_fake_process(&state, vm.id);
        let bootstrap_id = state.bootstraps.begin("ubuntu-26.04", "alpine-3.24", vm.id);
        state.bootstraps.set_status(bootstrap_id, BootstrapStatus::Running);

        let state_for_run = state.clone();
        let vm_id = vm.id;
        let handle = tokio::spawn(async move {
            run_bootstrap_script(&state_for_run, bootstrap_id, vm_id).await;
        });

        wait_for_console_subscriber(&console).await;
        console
            .push_output(format!("{}:0\n", BOOTSTRAP_DONE_SENTINEL).as_bytes())
            .await;
        handle.await.unwrap();

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Packaging);
    }
```
(Match this against however `handlers::builds`'s own test module actually names its fake console's output-injection method — check `register_fake_process`/`wait_for_console_subscriber` in `builds.rs`'s test module, copy the exact same local helpers into `bootstrap.rs`'s test module rather than trying to import them, since they're private to that file's `#[cfg(test)]` block; if the real method isn't named `push_output`, use whatever it actually is.)

Run: `cargo test -p firecrab-api handlers::bootstrap::tests::run_bootstrap_script -- --nocapture`
Expected: FAIL (`run_bootstrap_script` and `BOOTSTRAP_DONE_SENTINEL` don't exist).

- [ ] **Step 3: Implement `run_bootstrap_script`**

Add near the top of `handlers/bootstrap.rs`, with the other constants:
```rust
/// Sentinel the pushed script prints once it's done, followed by `:` and
/// its exit code — same shape as `packages::DONE_SENTINEL`, kept as its
/// own distinct string so a bootstrap's completion can never be confused
/// with an unrelated package action finishing on the same console.
const BOOTSTRAP_DONE_SENTINEL: &str = "FIRECRAB_BOOTSTRAP_DONE";

/// How long the guest-side bootstrap script may run before this module
/// gives up waiting — real network downloads (hundreds of MB) plus a real
/// package install, so far more generous than
/// `packages::PACKAGE_UPDATE_TIMEOUT`.
const BOOTSTRAP_SCRIPT_TIMEOUT: Duration = Duration::from_secs(1800);
```

Add the embedded script contents (read at compile time, so a typo in the shell script fails the Rust build rather than surfacing only at runtime against a real VM):
```rust
const ALPINE_SCRIPT: &str = include_str!("../../../scripts/firecracker-menual/bootstrap-alpine-in-guest.sh");
const UBUNTU_SCRIPT: &str = include_str!("../../../scripts/firecracker-menual/bootstrap-ubuntu-in-guest.sh");
const ROCKY_SCRIPT: &str = include_str!("../../../scripts/firecracker-menual/bootstrap-rocky-in-guest.sh");

fn script_for(alias: &str) -> &'static str {
    match alias {
        "alpine-3.24" => ALPINE_SCRIPT,
        "ubuntu-26.04" => UBUNTU_SCRIPT,
        "rocky-9" => ROCKY_SCRIPT,
        other => unreachable!("start_bootstrap already rejected unknown alias {other}"),
    }
}
```
(Verify the relative path — `../../../scripts/...` from `firecrab-api/src/handlers/bootstrap.rs` should resolve to the repo-root `scripts/` directory; adjust the `../` count if the actual directory depth differs, and confirm with `cargo build -p firecrab-api` that it compiles before moving on.)

Add the runner function:
```rust
/// Writes the guest-native bootstrap script for `state.bootstraps.get(id).alias`
/// to the builder VM's console as a single heredoc (so the whole script
/// lands as one shell invocation — no chunking or base64 needed, since
/// `write_input` writes raw bytes to the guest's stdin pipe and the
/// guest's own shell parses embedded newlines exactly the way it would
/// typed input, including multi-line constructs), waits for
/// [`BOOTSTRAP_DONE_SENTINEL`], and advances the session on success.
pub(crate) async fn run_bootstrap_script(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let Some(session) = state.bootstraps.get(bootstrap_id) else {
        return;
    };
    let script = script_for(&session.alias);

    let Some(process) = state
        .processes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&vm_id)
        .cloned()
    else {
        state.bootstraps.finish_err_from(
            bootstrap_id,
            BootstrapStatus::Running,
            "builder VM's console process is no longer available",
        );
        return;
    };

    let (_backlog, mut receiver) = process.console.subscribe();
    let heredoc = format!(
        "cat > /root/fc-bootstrap.sh <<'FIRECRAB_BOOTSTRAP_SCRIPT_EOF'\n{script}\nFIRECRAB_BOOTSTRAP_SCRIPT_EOF\nsh /root/fc-bootstrap.sh; echo \"{BOOTSTRAP_DONE_SENTINEL}:$?\"\n"
    );
    process.console.write_input(heredoc.as_bytes()).await;

    match super::packages::wait_for_completion_with_sentinel(
        &mut receiver,
        BOOTSTRAP_SCRIPT_TIMEOUT,
        BOOTSTRAP_DONE_SENTINEL,
    )
    .await
    {
        Ok((0, tail)) => {
            state.bootstraps.append_log(bootstrap_id, tail);
            if state
                .bootstraps
                .set_status_from(bootstrap_id, BootstrapStatus::Running, BootstrapStatus::Packaging)
            {
                let state_for_package = state.clone();
                tokio::spawn(async move {
                    package_bootstrap(&state_for_package, bootstrap_id, vm_id).await;
                });
            }
        }
        Ok((code, tail)) => {
            state.bootstraps.finish_err_from(
                bootstrap_id,
                BootstrapStatus::Running,
                format!("bootstrap script exited with code {code}\n{tail}"),
            );
        }
        Err(reason) => {
            state
                .bootstraps
                .finish_err_from(bootstrap_id, BootstrapStatus::Running, reason);
        }
    }
}
```

This calls `packages::wait_for_completion_with_sentinel` — a small generalization of the existing `wait_for_completion`/`find_done_sentinel`, which currently hardcode `packages::DONE_SENTINEL`. Go back to `firecrab-api/src/handlers/packages.rs` and parameterize the sentinel:
```rust
pub(crate) async fn wait_for_completion(
    receiver: &mut broadcast::Receiver<Vec<u8>>,
    timeout: Duration,
) -> Result<(i32, String), String> {
    wait_for_completion_with_sentinel(receiver, timeout, DONE_SENTINEL).await
}

/// Same as [`wait_for_completion`] but against an arbitrary sentinel string
/// — `handlers::bootstrap` uses its own distinct sentinel so a bootstrap's
/// completion can never be confused with a package action's.
pub(crate) async fn wait_for_completion_with_sentinel(
    receiver: &mut broadcast::Receiver<Vec<u8>>,
    timeout: Duration,
    sentinel: &str,
) -> Result<(i32, String), String> {
    let mut tail = Vec::new();
    let wait = async {
        loop {
            match receiver.recv().await {
                Ok(chunk) => {
                    tail.extend_from_slice(&chunk);
                    if tail.len() > OUTPUT_TAIL_CAP {
                        let excess = tail.len() - OUTPUT_TAIL_CAP;
                        tail.drain(..excess);
                    }
                    if let Some(code) = find_sentinel(&tail, sentinel) {
                        return Ok((code, String::from_utf8_lossy(&tail).into_owned()));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err("console closed before the command finished".to_owned());
                }
            }
        }
    };

    tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or_else(|_| Err("timed out waiting for the command to finish".to_owned()))
}
```
Rename the existing `find_done_sentinel` to a sentinel-parameterized `find_sentinel`:
```rust
pub(crate) fn find_sentinel(buffer: &[u8], sentinel: &str) -> Option<i32> {
    let text = String::from_utf8_lossy(buffer);
    text.lines().rev().find_map(|line| {
        let (_, rest) = line.split_once(sentinel)?;
        rest.trim_start_matches(':').trim().parse().ok()
    })
}
```
Update the one existing call site inside this same file (`run_action`'s call to `wait_for_completion`) — no change needed there since `wait_for_completion` keeps its old signature and now just delegates. Update `packages.rs`'s own tests if any call `find_done_sentinel` directly — rename those call sites to `find_sentinel(buffer, DONE_SENTINEL)`.

`package_bootstrap` is Task 8 — for this task, stub it so the crate compiles:
```rust
async fn package_bootstrap(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let _ = vm_id;
    // Implemented in Task 8.
    state.bootstraps.finish_err(bootstrap_id, "packaging not yet implemented");
}
```
(Task 8 replaces this stub with the real implementation.)

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p firecrab-api handlers::bootstrap:: handlers::packages:: -- --nocapture`
Expected: PASS, including any renamed `find_sentinel` call sites in `packages.rs`'s own tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
git add firecrab-api/src/handlers/bootstrap.rs firecrab-api/src/handlers/packages.rs
git commit -m "feat: push the bootstrap script over the console and wait for completion"
```

---

### Task 8: Extract the built rootfs/kernel from the guest disk and package it

**Files:**
- Modify: `firecrab-api/src/handlers/bootstrap.rs`
- Modify: `firecrab-api/src/server.rs` (nothing new here yet — Task 9 adds `GET`/`DELETE`)

**Interfaces:**
- Consumes: `rootfs::dump_from_image` (Task 2), `image_install::{staged_package_path, package_name}` (existing), `templates::TemplateRegistry::known_spec` (existing), `scripts/firecracker-menual/extract-vmlinux` (existing, invoked as a subprocess).
- Produces: replaces the Task 7 stub of `package_bootstrap` with the real implementation — the session's terminal action.

- [ ] **Step 1: Write the failing test**

Add to `handlers/bootstrap.rs`'s test module:
```rust
    #[tokio::test]
    async fn package_bootstrap_writes_a_tar_zst_the_install_pipeline_can_read() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = crate::handlers::vms::test_support::record("builder", Uuid::new_v4());
        crate::handlers::vms::test_support::seed_vm(&state, &vm);
        let generation = Uuid::new_v4();
        let artifact_paths = crate::artifacts::VmArtifactPaths::for_vm(
            &state.vms_dir_for(&vm.storage_root),
            vm.id,
        );
        artifact_paths.ensure_directories().unwrap();
        let disk_path = artifact_paths.rootfs(generation);

        // Build a small ext4 image on this disk path containing the exact
        // guest-side layout package_bootstrap expects to find and dump out.
        std::process::Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&disk_path)
            .arg("16M")
            .status()
            .unwrap();
        crate::rootfs::write_into_image(&disk_path, "/root/fc-bootstrap/out/rootfs.ext4", b"fake ext4 rootfs bytes").unwrap();
        crate::rootfs::write_into_image(&disk_path, "/root/fc-bootstrap/out/vmlinuz-raw", b"fake vmlinux elf bytes").unwrap();
        {
            let mut vms = state.vms.lock().unwrap();
            let stored = vms.get_mut(&vm.id).unwrap();
            stored.disk_generation = Some(generation);
        }

        let bootstrap_id = state.bootstraps.begin("ubuntu-26.04", "alpine-3.24", vm.id);
        state.bootstraps.set_status(bootstrap_id, BootstrapStatus::Packaging);

        package_bootstrap(&state, bootstrap_id, vm.id).await;

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Succeeded);
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "ubuntu-26.04",
        );
        assert!(staged.is_file());
    }
```
This test calls `crate::rootfs::write_into_image` directly to seed the fixture disk with the exact guest-side output paths a real script run would have left behind (`/root/fc-bootstrap/out/rootfs.ext4`, `/root/fc-bootstrap/out/vmlinuz-raw`) — `write_into_image` is currently private (`fn`, not `pub`/`pub(crate)`) in `rootfs.rs`, so as part of this step widen it to `pub(crate)` the same way Task 3 widened other single-file-scoped helpers for cross-module reuse. This test only seeds `rootfs.ext4` and `vmlinuz-raw` (omitting `initramfs`, matching the `None` initrd case — `ubuntu-26.04` in `default_specs()` has `initrd: None`), so `build_package_blocking`'s `if let Some(initrd_relative) = &spec.initrd` branch is exercised as *not* taken here; add a second test targeting `alpine-3.24` (which does have an initrd) if you want that branch covered too — not required for this task's own tests to pass, but worth doing given the `if let` branch would otherwise ship untested.

Run: `cargo test -p firecrab-api handlers::bootstrap::tests::package_bootstrap -- --nocapture`
Expected: FAIL.

- [ ] **Step 2: Implement `package_bootstrap`**

Replace the Task 7 stub with:
```rust
/// Dumps the finished rootfs/kernel/initrd out of the builder VM's disk,
/// converts the raw kernel to an uncompressed ELF vmlinux the same way
/// `install-{alpine,ubuntu,rocky}-rootfs.sh` always did on the host, packs
/// everything into `{alias}.tar.zst` at the exact layout
/// `templates.rs::default_specs()` expects, writes it to
/// `image_install::staged_package_path` — the unmodified "가져오기"
/// pipeline picks it up from there — then deletes the builder VM.
async fn package_bootstrap(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let Some(session) = state.bootstraps.get(bootstrap_id) else {
        return;
    };
    let Some(spec) = TemplateRegistry::known_spec(&session.alias) else {
        state.bootstraps.finish_err(bootstrap_id, format!("no known spec for {}", session.alias));
        return;
    };

    let result = package_bootstrap_inner(state, &session, &spec).await;

    let _ = super::vms::delete_vm(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(vm_id.to_string()),
    )
    .await;

    match result {
        Ok(()) => state.bootstraps.finish_ok(bootstrap_id),
        Err(reason) => state.bootstraps.finish_err(bootstrap_id, reason),
    }
}

async fn package_bootstrap_inner(
    state: &AppState,
    session: &BootstrapResponse,
    spec: &crate::templates::TemplateSpec,
) -> Result<(), String> {
    let vm_record = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&session.vm_id)
        .cloned()
        .ok_or_else(|| "builder VM record vanished before packaging".to_owned())?;
    let disk_generation = vm_record
        .disk_generation
        .ok_or_else(|| "builder VM has no disk generation to package from".to_owned())?;
    let artifact_paths = crate::artifacts::VmArtifactPaths::for_vm(
        &state.vms_dir_for(&vm_record.storage_root),
        session.vm_id,
    );
    let guest_disk = artifact_paths.rootfs(disk_generation);
    let alias = session.alias.clone();
    let spec = spec.clone();
    let image_root = state.templates.image_root_path().to_path_buf();

    tokio::task::spawn_blocking(move || build_package_blocking(&guest_disk, &alias, &spec, &image_root))
        .await
        .map_err(|error| format!("packaging task panicked: {error}"))?
}

/// The synchronous half of packaging: dump files, convert the kernel,
/// stage them under a scratch directory in the right `kernel/`/`rootfs/`
/// layout, tar+zstd, then publish atomically into the staged package
/// cache (temp-file-then-rename, same discipline as
/// `image_install::download_to`).
fn build_package_blocking(
    guest_disk: &std::path::Path,
    alias: &str,
    spec: &crate::templates::TemplateSpec,
    image_root: &std::path::Path,
) -> Result<(), String> {
    let scratch = image_root.join(".packages").join(format!(".{alias}-bootstrap-scratch"));
    let _ = std::fs::remove_dir_all(&scratch);
    let kernel_dir = scratch.join("kernel");
    let rootfs_dir = scratch.join("rootfs");
    std::fs::create_dir_all(&kernel_dir).map_err(|e| format!("mkdir {}: {e}", kernel_dir.display()))?;
    std::fs::create_dir_all(&rootfs_dir).map_err(|e| format!("mkdir {}: {e}", rootfs_dir.display()))?;

    let raw_rootfs = scratch.join("rootfs.raw.ext4");
    crate::rootfs::dump_from_image(guest_disk, "/root/fc-bootstrap/out/rootfs.ext4", &raw_rootfs)
        .map_err(|e| format!("dump rootfs: {e}"))?;
    let rootfs_dest = scratch.join(&spec.rootfs); // spec.rootfs = "rootfs/<exact-filename>.ext4"
    std::fs::create_dir_all(rootfs_dest.parent().unwrap()).ok();
    std::fs::rename(&raw_rootfs, &rootfs_dest).map_err(|e| format!("place rootfs: {e}"))?;

    let raw_kernel_name = if alias == "alpine-3.24" { "vmlinuz-virt-raw" } else { "vmlinuz-raw" };
    let raw_kernel = scratch.join("kernel.raw");
    crate::rootfs::dump_from_image(
        guest_disk,
        &format!("/root/fc-bootstrap/out/{raw_kernel_name}"),
        &raw_kernel,
    )
    .map_err(|e| format!("dump kernel: {e}"))?;

    let kernel_dest = scratch.join(&spec.kernel); // e.g. "kernel/vmlinux-ubuntu-26.04-x86_64"
    std::fs::create_dir_all(kernel_dest.parent().unwrap()).ok();
    let extract_vmlinux = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join("scripts/firecracker-menual/extract-vmlinux");
    let output = std::process::Command::new(&extract_vmlinux)
        .arg(&raw_kernel)
        .output()
        .map_err(|e| format!("run extract-vmlinux: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "extract-vmlinux failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    std::fs::write(&kernel_dest, &output.stdout).map_err(|e| format!("write kernel: {e}"))?;

    if let Some(initrd_relative) = &spec.initrd {
        let raw_initrd = scratch.join("initramfs.raw");
        crate::rootfs::dump_from_image(guest_disk, "/root/fc-bootstrap/out/initramfs", &raw_initrd)
            .map_err(|e| format!("dump initrd: {e}"))?;
        let initrd_dest = scratch.join(initrd_relative);
        std::fs::create_dir_all(initrd_dest.parent().unwrap()).ok();
        std::fs::rename(&raw_initrd, &initrd_dest).map_err(|e| format!("place initrd: {e}"))?;
    }

    let package_name = crate::image_install::package_name(alias);
    let staged = crate::image_install::staged_package_path(image_root, alias);
    let staging_temp = staged.with_file_name(format!(".{package_name}.building"));
    std::fs::create_dir_all(staged.parent().unwrap()).ok();

    let members: Vec<std::path::PathBuf> = if spec.initrd.is_some() {
        vec![spec.kernel.clone(), spec.initrd.clone().unwrap(), spec.rootfs.clone()]
    } else {
        vec![spec.kernel.clone(), spec.rootfs.clone()]
    };
    let mut tar = std::process::Command::new("tar")
        .arg("--sparse")
        .arg("-C")
        .arg(&scratch)
        .arg("-cf")
        .arg("-")
        .args(&members)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn tar: {e}"))?;
    let tar_stdout = tar.stdout.take().ok_or("tar stdout missing")?;
    let zstd = std::process::Command::new("zstd")
        .args(["-T0", "-19", "-f", "-o"])
        .arg(&staging_temp)
        .stdin(tar_stdout)
        .status()
        .map_err(|e| format!("run zstd: {e}"))?;
    let tar_status = tar.wait().map_err(|e| format!("tar wait: {e}"))?;
    if !tar_status.success() || !zstd.success() {
        let _ = std::fs::remove_file(&staging_temp);
        return Err(format!("packaging failed (tar {tar_status}, zstd {zstd})"));
    }

    std::fs::rename(&staging_temp, &staged).map_err(|e| format!("publish package: {e}"))?;
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}
```

Check `AppState::vms_dir_for` and `RequestId`'s constructor (`RequestId(Uuid::new_v4())`) against their real current signatures in `state.rs`/`server.rs` before finalizing — both are already used exactly this way in `handlers::builds::finalize_and_register`, so mirror that call site precisely rather than guessing.

- [ ] **Step 3: Run tests, verify pass**

Run: `cargo test -p firecrab-api handlers::bootstrap:: -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Manual end-to-end verification**

This is the first point in the plan where the full pipeline can be exercised for real. With the backend running and at least one template already installed (`ubuntu-26.04`, say):
```bash
curl -X POST http://localhost:3000/api/images/alpine-3.24/bootstrap
# poll:
curl http://localhost:3000/api/images/bootstrap/<bootstrapId>
```
Watch the log reach `succeeded`, then confirm `images/.packages/alpine-3.24.tar.zst` exists and `tar --use-compress-program=zstd -tf` lists exactly `kernel/vmlinux-alpine-virt-x86_64`, `kernel/initramfs-alpine-virt-x86_64`, `rootfs/alpine-rootfs-3.24.1-x86_64.ext4`. This step will surface any real bug in Task 6's guest scripts — fix the specific script, not this task's Rust code, if the failure is inside the guest.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
git add firecrab-api/src/handlers/bootstrap.rs firecrab-api/src/rootfs.rs
git commit -m "feat: extract and package a bootstrap session's built rootfs/kernel"
```

---

### Task 9: `GET`/`DELETE /api/images/bootstrap/{bootstrapId}`

**Files:**
- Modify: `firecrab-api/src/handlers/bootstrap.rs`
- Modify: `firecrab-api/src/server.rs`

**Interfaces:**
- Produces: `pub async fn get_bootstrap(...)`, `pub async fn cancel_bootstrap(...)`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn get_bootstrap_returns_the_tracked_session() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let id = state.bootstraps.begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4());

        let Json(found) = get_bootstrap(State(state), Path(id.to_string())).await.unwrap();

        assert_eq!(found.bootstrap_id, id);
    }

    #[tokio::test]
    async fn get_bootstrap_404s_for_an_unknown_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = get_bootstrap(State(state), Path(Uuid::new_v4().to_string()))
            .await
            .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cancel_bootstrap_deletes_the_builder_vm_and_drops_the_session() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = crate::handlers::vms::test_support::record("builder", Uuid::new_v4());
        crate::handlers::vms::test_support::seed_vm(&state, &vm);
        let id = state.bootstraps.begin("ubuntu-26.04", "alpine-3.24", vm.id);

        let status = cancel_bootstrap(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(state.bootstraps.get(id).is_none());
    }
```
(`get_bootstrap` deliberately takes only `State`+`Path`, no `Extension<RequestId>`/`AppError::not_found` needing a request id — check `handlers::builds::get_build`'s actual signature first; if it DOES take `Extension<RequestId>` for its `not_found` call, match that exactly instead, since `AppError::not_found(request_id.0)` requires one — don't diverge from the established signature shape without a reason.)

Run: `cargo test -p firecrab-api handlers::bootstrap::tests::get_bootstrap handlers::bootstrap::tests::cancel_bootstrap -- --nocapture`
Expected: FAIL.

- [ ] **Step 2: Implement both handlers**

```rust
/// `GET /api/images/bootstrap/{bootstrapId}`.
pub async fn get_bootstrap(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<BootstrapResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    state
        .bootstraps
        .get(id)
        .map(Json)
        .ok_or_else(|| AppError::not_found(request_id.0))
}

/// `DELETE /api/images/bootstrap/{bootstrapId}` — tears down the builder VM
/// without packaging anything. Mirrors `handlers::builds::cancel_build`
/// exactly (same stop-then-delete-if-needed sequence, same best-effort
/// swallowing of VM teardown errors).
pub async fn cancel_bootstrap(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let parsed_id = parse_id(&id, request_id.0)?;
    let session = state
        .bootstraps
        .get(parsed_id)
        .ok_or_else(|| AppError::not_found(request_id.0))?;

    let can_delete_now = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&session.vm_id)
        .map(|vm| vm.state.can_delete())
        .unwrap_or(true);
    if !can_delete_now {
        let _ = super::vms::stop_vm(
            State(state.clone()),
            Extension(request_id),
            Path(session.vm_id.to_string()),
        )
        .await;
    }

    let _ = super::vms::delete_vm(
        State(state.clone()),
        Extension(request_id),
        Path(session.vm_id.to_string()),
    )
    .await;

    state.bootstraps.remove(parsed_id);
    Ok(StatusCode::NO_CONTENT)
}
```
(Adjust `get_bootstrap`'s signature to match whatever `get_build`'s real signature turns out to be, per the Step 1 note — if it doesn't take `Extension<RequestId>`, drop that parameter and the `AppError::not_found(request_id.0)` call accordingly.)

- [ ] **Step 3: Run tests, verify pass**

Run: `cargo test -p firecrab-api handlers::bootstrap:: -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Wire the routes**

In `server.rs`:
```rust
        .route(
            "/api/images/bootstrap/{bootstrapId}",
            get(handlers::bootstrap::get_bootstrap).delete(handlers::bootstrap::cancel_bootstrap),
        )
```

- [ ] **Step 5: Full backend check + commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
cargo test -p firecrab-api -p firecrab-api-types
git add firecrab-api/src/handlers/bootstrap.rs firecrab-api/src/server.rs
git commit -m "feat: add GET/DELETE /api/images/bootstrap/{bootstrapId}"
```

---

### Task 10: Frontend — client functions

**Files:**
- Modify: `firecrab-frontend/src/api/client.ts`

**Interfaces:**
- Consumes: Task 1's bindings, Task 5-9's routes.
- Produces: `startBootstrap`, `getBootstrap`, `cancelBootstrap` — consumed by Task 11.

- [ ] **Step 1: Add the functions**

Near the existing build-session functions:
```ts
/** Bootstrap a distro from scratch inside a builder VM (`POST /api/images/{alias}/bootstrap`). */
export function startBootstrap(alias: string): Promise<BootstrapResponse> {
  return fetchJson(`/api/images/${encodeURIComponent(alias)}/bootstrap`, { method: "POST" });
}

/** Poll one bootstrap session (`GET /api/images/bootstrap/{bootstrapId}`). */
export function getBootstrap(bootstrapId: string): Promise<BootstrapResponse> {
  return fetchJson(`/api/images/bootstrap/${encodeURIComponent(bootstrapId)}`);
}

/** Cancel a bootstrap and delete its builder VM (`DELETE /api/images/bootstrap/{bootstrapId}`). */
export async function cancelBootstrap(bootstrapId: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/images/bootstrap/${encodeURIComponent(bootstrapId)}`, { method: "DELETE" });
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
}
```
Add `BootstrapResponse` to the existing `import type { ... } from "../bindings"` line.

- [ ] **Step 2: Type-check**

Run: `cd firecrab-frontend && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add firecrab-frontend/src/api/client.ts
git commit -m "feat(frontend): add bootstrap session API client functions"
```

---

### Task 11: Frontend — "부트스트랩 빌드" button + log panel

**Files:**
- Modify: `firecrab-frontend/src/components/Images.tsx`

**Interfaces:**
- Consumes: Task 10's client functions.
- Produces: no new exports — a self-contained UI addition to the existing `Images` component.

- [ ] **Step 1: Read the current file in full**

Read `firecrab-frontend/src/components/Images.tsx` end to end before editing — it was substantially rewritten this session (Task 12/13 of the prior plan); confirm the exact current shape of the table row JSX, `KNOWN_TEMPLATES`, and `BuildModal` before adding to it.

- [ ] **Step 2: Add a `BootstrapPanel` component and wire it in**

Add near `BuildModal`:
```tsx
/**
 * Bootstrap panel: triggers a from-scratch distro bootstrap and shows its
 * live log. Only one bootstrap runs at a time (backend-enforced, 409 on a
 * second start) — this component's own busy state mirrors that by polling
 * `getBootstrap` and disabling every alias's button while any session it
 * knows about is non-terminal.
 */
function BootstrapPanel({ onFinished }: { onFinished: () => void }) {
  const [session, setSession] = useState<BootstrapResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!session || session.status === "succeeded" || session.status === "failed") return;
    let cancelled = false;
    const timer = setTimeout(async () => {
      try {
        const snapshot = await getBootstrap(session.bootstrapId);
        if (!cancelled) {
          setSession(snapshot);
          if (snapshot.status === "succeeded") onFinished();
        }
      } catch {
        /* keep last snapshot */
      }
    }, 1000);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [session, onFinished]);

  const start = async (alias: string) => {
    setError(null);
    try {
      const started = await startBootstrap(alias);
      setSession(started);
    } catch (err) {
      setError((err as Error).message);
    }
  };

  const busy = session !== null && session.status !== "succeeded" && session.status !== "failed";

  return (
    <section className="panel">
      <h2 className="panel-title">배포판 부트스트랩</h2>
      {error && <div className="field-error">{error}</div>}
      <div className="package-row">
        {(["alpine-3.24", "ubuntu-26.04", "rocky-9"] as const).map((alias) => (
          <button
            key={alias}
            type="button"
            className="btn"
            disabled={busy}
            onClick={() => void start(alias)}
          >
            {busy && session?.alias === alias ? `${alias} 부트스트랩 중…` : `${alias} 부트스트랩`}
          </button>
        ))}
      </div>
      {session && (
        <>
          <div className="state-badge">{session.status}</div>
          <pre className="detail-log">{session.log}</pre>
        </>
      )}
    </section>
  );
}
```

In the `Images` component's returned JSX, render `<BootstrapPanel onFinished={() => void refreshList()} />` — place it near where the existing "+ 새 이미지 빌드" control lives, since both are "start something new" affordances on this same screen.

Add `startBootstrap, getBootstrap` to the existing `../api/client` import, and `BootstrapResponse` to the `../bindings` import.

- [ ] **Step 3: Type-check**

Run: `cd firecrab-frontend && npx tsc --noEmit && npm run build`
Expected: clean.

- [ ] **Step 4: Manual browser verification**

Start the backend and frontend (see the project's `run` skill), open the Images screen, click "alpine-3.24 부트스트랩" with at least one other template already installed, and confirm: the log panel updates roughly every second, the other two buttons stay disabled while it runs, and once it succeeds the alpine-3.24 row's "가져오기" button becomes clickable (since `images/.packages/alpine-3.24.tar.zst` now exists) and completes the install.

- [ ] **Step 5: Commit**

```bash
git add firecrab-frontend/src/components/Images.tsx
git commit -m "feat(frontend): add bootstrap-from-scratch panel to the Images screen"
```

---

### Task 12: Docs

**Files:**
- Modify: `docs/20-guides/m2image-builder.md`

**Interfaces:** None — docs only.

- [ ] **Step 1: Add a section**

After the "웹에서 파생 이미지 빌드" section added by the prior plan:
```markdown
## 웹에서 배포판 부트스트랩

`build-m2images.sh`가 하던 일(공식 소스로부터 배포판을 처음부터 준비)을
docker나 sudo 없이 웹에서 트리거할 수 있다 — builder microVM 안에서
공식 base를 내려받아 chroot로 들어가 그 배포판 자신의 패키지 매니저로
패키지·커널을 설치하고, `mkfs.ext4 -d`로 완성된 rootfs를 만든다.

1. Images 화면에서 "배포판 부트스트랩" 아래 원하는 alias 클릭
2. 이미 설치된 임의의 템플릿으로 builder VM이 뜨고, 콘솔에서 부트스트랩
   스크립트가 실행됨 (로그가 실시간으로 표시됨)
3. 완료되면 `images/.packages/{alias}.tar.zst`가 생기고, 기존 "가져오기"
   버튼이 바로 활성화됨

동시에 하나의 부트스트랩만 진행할 수 있다. `rocky-9` 부트스트랩은
`dnf`가 필요해 builder VM 자체가 이미 `rocky-9`여야 한다 — 나머지
alpine-3.24/ubuntu-26.04는 이미 설치된 아무 템플릿에서나 부트스트랩
가능하다. 완전히 새로운 배포판(현재 3개 외)을 추가하는 것은 여전히 이
문서 위쪽의 CLI 경로를 쓴다.
```

- [ ] **Step 2: Commit**

```bash
git add docs/20-guides/m2image-builder.md
git commit -m "docs: document web-triggered distro bootstrap"
```

---

## Final Verification

After all 12 tasks:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd firecrab-frontend && npm run build
```
Then, with at least one template already installed:
- Bootstrap each of the 3 aliases from the web UI in turn (rocky-9 last, since it needs itself already installed — bootstrap alpine or ubuntu first, install it via 가져오기, then use it or an existing template as rocky's own eventual replacement source once rocky-9 exists).
- Confirm a second bootstrap request while one is running gets 409.
- Confirm `GET /api/vms` never lists a bootstrap's builder VM, even mid-run.
- Confirm cancelling a bootstrap mid-run leaves no orphaned builder VM.
- Run `cargo-llvm-cov` per project convention (memory: patch coverage was gated at 78% on a prior PR) if this work is heading toward review.
