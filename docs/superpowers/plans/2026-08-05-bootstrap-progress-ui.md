# Bootstrap Progress UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the bootstrap panel's opaque status badge + flat log with a four-stage timeline mirroring the VM startup stepper, plus a live inline terminal showing the builder VM's real console output while it runs.

**Architecture:** The backend gains a `BootstrapStep`/`BootstrapStepRun` timeline recorded inside `BootstrapTracker` (in-memory only, exactly like `VmRecord::startup_timeline`), threaded through the four transition points that already exist in `handlers/bootstrap.rs`. The frontend renders that timeline with a stepper copied from `VmDetailModal`'s `PipelineStepper` pattern, and mounts a stripped-down xterm component — reusing the already-shipping `/ws/vms/{id}/console` endpoint against the builder VM id — only while the session is non-terminal.

**Tech Stack:** Rust (axum, tokio, serde), React 19 + TypeScript, `@xterm/xterm` + `@xterm/addon-fit` (both already dependencies).

## Global Constraints

- Design doc: `docs/superpowers/specs/2026-08-05-bootstrap-progress-ui-design.md`. Every task implements part of it; nothing outside it.
- The step timeline is **in-memory only**. `BootstrapTracker` has no SQLite backing and this plan does not add one — same ruling as `VmRecord::startup_timeline` (see `docs/30-tasks/task-vm-startup-timeline.md`).
- **Session log lines are English.** UI labels (stepper box names, badges, buttons) stay Korean. The user's request was to remove Korean from the *log body*, not from UI chrome.
- `firecrab-frontend/src/bindings/*.ts` are **hand-maintained**. There is no ts-rs codegen in this workspace despite the header comment on older files. Every new Rust wire type needs a matching hand-written `.ts` file plus an `export * from "./X"` line in `bindings/index.ts`.
- New wire types follow the existing convention in `firecrab-api-types/src/lib.rs`: `#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]` and `#[serde(rename_all = "camelCase")]` for the enums, `#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]` + `#[serde(rename_all = "camelCase")]` for the struct.
- Run after every Rust task: `cargo fmt --all`, `cargo clippy -p firecrab-api --all-targets`, `cargo test -p firecrab-api`. Clippy has pre-existing warnings in `ipam.rs`, `storage.rs`, `network.rs`, `image_install.rs` — do not "fix" those; only ensure you add none.
- Run after every frontend task: `cd firecrab-frontend && npm run build` (this runs `tsc` then `vite build`).
- Commit at the end of every task. Do not squash tasks together.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `firecrab-api-types/src/lib.rs` | `BootstrapStep`, `BootstrapStepOutcome`, `BootstrapStepRun` + two new `BootstrapResponse` fields | 1 |
| `firecrab-frontend/src/bindings/BootstrapStep.ts` | TS mirror of the step enum | 1 |
| `firecrab-frontend/src/bindings/BootstrapStepOutcome.ts` | TS mirror of the outcome enum | 1 |
| `firecrab-frontend/src/bindings/BootstrapStepRun.ts` | TS mirror of the run struct | 1 |
| `firecrab-frontend/src/bindings/BootstrapResponse.ts` | Add the two new fields | 1 |
| `firecrab-frontend/src/bindings/index.ts` | Re-export the three new binding files | 1 |
| `firecrab-api/src/bootstrap.rs` | `set_step` / `close_open_step`, auto-close inside `finish_ok`/`finish_err`/`finish_err_from`, elapsed-time `clock()` | 2, 3 |
| `firecrab-api/src/handlers/bootstrap.rs` | Call `set_step` at the three non-initial transitions; English heartbeat | 4, 5 |
| `firecrab-frontend/src/components/InlineConsole.tsx` | Stripped-down live xterm bound to one `vmId` | 6 |
| `firecrab-frontend/src/components/Images.tsx` | `BootstrapStepper` + wiring both new pieces into `BootstrapPanel` | 7 |
| `firecrab-frontend/src/index.css` | Styles for the inline console container | 6 |

---

### Task 1: Wire types for the bootstrap step timeline

**Files:**
- Modify: `firecrab-api-types/src/lib.rs` (add three types near `BootstrapStatus` at ~line 730; add two fields to `BootstrapResponse` at ~line 752)
- Create: `firecrab-frontend/src/bindings/BootstrapStep.ts`
- Create: `firecrab-frontend/src/bindings/BootstrapStepOutcome.ts`
- Create: `firecrab-frontend/src/bindings/BootstrapStepRun.ts`
- Modify: `firecrab-frontend/src/bindings/BootstrapResponse.ts`
- Modify: `firecrab-frontend/src/bindings/index.ts`
- Modify: `firecrab-api/src/bootstrap.rs` (only to populate the two new fields where `BootstrapResponse` is constructed — the crate won't compile otherwise)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `firecrab_api_types::BootstrapStep` — unit enum, variants `StartingBuilderVm`, `InstallingSystem`, `Packaging`, `Finalizing`. Serializes camelCase (`"startingBuilderVm"`, …).
  - `firecrab_api_types::BootstrapStepOutcome` — unit enum, variants `Running`, `Succeeded`, `Failed`. Serializes camelCase.
  - `firecrab_api_types::BootstrapStepRun { step: BootstrapStep, started_at_ms: u64, ended_at_ms: Option<u64>, outcome: BootstrapStepOutcome, detail: Option<String> }`.
  - `BootstrapResponse.current_step: Option<BootstrapStep>` and `BootstrapResponse.step_timeline: Vec<BootstrapStepRun>`.
  - TS: `BootstrapStep`, `BootstrapStepOutcome`, `BootstrapStepRun`, and `BootstrapResponse.currentStep` / `.stepTimeline`.

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` at the bottom of `firecrab-api-types/src/lib.rs`:

```rust
#[test]
fn bootstrap_step_run_serializes_camel_case_for_the_dashboard() {
    let run = BootstrapStepRun {
        step: BootstrapStep::InstallingSystem,
        started_at_ms: 1_700_000_000_000,
        ended_at_ms: None,
        outcome: BootstrapStepOutcome::Running,
        detail: None,
    };
    let json = serde_json::to_value(&run).expect("serialize");
    assert_eq!(json["step"], "installingSystem");
    assert_eq!(json["startedAtMs"], 1_700_000_000_000_u64);
    assert_eq!(json["endedAtMs"], serde_json::Value::Null);
    assert_eq!(json["outcome"], "running");
}

#[test]
fn bootstrap_response_carries_an_empty_timeline_by_default() {
    let json = serde_json::json!({
        "bootstrapId": "00000000-0000-0000-0000-000000000000",
        "alias": "alpine-3.24",
        "sourceAlias": "__microboot",
        "vmId": "00000000-0000-0000-0000-000000000000",
        "status": "booting",
        "log": "",
        "startedAtMs": 0,
    });
    let parsed: BootstrapResponse = serde_json::from_value(json).expect("deserialize");
    assert!(parsed.step_timeline.is_empty());
    assert_eq!(parsed.current_step, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p firecrab-api-types bootstrap_step`
Expected: FAIL — `cannot find type BootstrapStepRun in this scope`.

- [ ] **Step 3: Add the Rust types**

Insert immediately after the `BootstrapStatus` enum in `firecrab-api-types/src/lib.rs`:

```rust
/// A named phase of one bootstrap session, exposed so the dashboard can
/// show *where* a multi-minute run is instead of a single opaque status.
/// Deliberately coarser than the code's own phase boundaries — four boxes
/// that mean something to an operator, mirroring [`StartupStep`]'s four —
/// with the fine-grained detail of the longest one left to the live
/// console instead of more enum variants
/// (`docs/superpowers/specs/2026-08-05-bootstrap-progress-ui-design.md`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BootstrapStep {
    /// Resolving the MicroBoot builder source, creating the builder VM,
    /// and waiting for a shell to answer on its console.
    StartingBuilderVm,
    /// The guest script is running: download, chroot install, mkfs. By far
    /// the longest phase, and the one the live console is for.
    InstallingSystem,
    /// Builder VM stopped; its disk is being dumped and compressed into
    /// `{alias}.tar.zst`.
    Packaging,
    /// Package staged; tearing the builder VM down.
    Finalizing,
}

/// How one [`BootstrapStep`] ended, or that it hasn't yet. Structurally
/// identical to [`StartupStepOutcome`] but kept separate, matching how
/// `BootstrapStatus` and `VmState` are separate types rather than one
/// shared lifecycle enum.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BootstrapStepOutcome {
    /// Still in progress — `ended_at_ms` is `None`.
    Running,
    /// Finished and moved on to the next step.
    Succeeded,
    /// The session failed here. No later step ever began.
    Failed,
}

/// One pass through a [`BootstrapStep`], with the wall-clock times it
/// spanned. Server-timed for the same reason [`StartupStepRun`] is: the
/// dashboard's poll interval is far coarser than the fastest steps take.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapStepRun {
    pub step: BootstrapStep,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub outcome: BootstrapStepOutcome,
    /// Failure reason, only ever set on a `Failed` step.
    pub detail: Option<String>,
}
```

Then add these two fields to `BootstrapResponse`, immediately after `pub status: BootstrapStatus,`:

```rust
    /// The step currently open, `None` once the session is terminal.
    #[serde(default)]
    pub current_step: Option<BootstrapStep>,
    /// Every step this session has entered, in order. `#[serde(default)]`
    /// so a dashboard talking to an older server still deserializes.
    #[serde(default)]
    pub step_timeline: Vec<BootstrapStepRun>,
```

- [ ] **Step 4: Fix the one construction site so the API crate compiles**

In `firecrab-api/src/bootstrap.rs`, `insert_session` builds a `BootstrapResponse` literal. Add the two fields after `status: BootstrapStatus::Booting,`:

```rust
            current_step: None,
            step_timeline: Vec::new(),
```

(Task 2 replaces these with a real opening step; this is only to keep the tree compiling.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p firecrab-api-types bootstrap_step && cargo test -p firecrab-api-types bootstrap_response && cargo test -p firecrab-api`
Expected: PASS, all of them.

- [ ] **Step 6: Write the TypeScript bindings**

`firecrab-frontend/src/bindings/BootstrapStep.ts`:

```ts
// Mirrors firecrab_api_types::BootstrapStep.
export type BootstrapStep =
  | "startingBuilderVm"
  | "installingSystem"
  | "packaging"
  | "finalizing";
```

`firecrab-frontend/src/bindings/BootstrapStepOutcome.ts`:

```ts
// Mirrors firecrab_api_types::BootstrapStepOutcome.
export type BootstrapStepOutcome = "running" | "succeeded" | "failed";
```

`firecrab-frontend/src/bindings/BootstrapStepRun.ts`:

```ts
// Mirrors firecrab_api_types::BootstrapStepRun.
import type { BootstrapStep } from "./BootstrapStep";
import type { BootstrapStepOutcome } from "./BootstrapStepOutcome";

export type BootstrapStepRun = {
  step: BootstrapStep;
  startedAtMs: number;
  endedAtMs: number | null;
  outcome: BootstrapStepOutcome;
  detail: string | null;
};
```

In `firecrab-frontend/src/bindings/BootstrapResponse.ts`, add the import and the two fields (keep the file's existing style — read it first and match it):

```ts
import type { BootstrapStep } from "./BootstrapStep";
import type { BootstrapStepRun } from "./BootstrapStepRun";
```

and inside the type body, after `status: BootstrapStatus;`:

```ts
  currentStep: BootstrapStep | null;
  stepTimeline: BootstrapStepRun[];
```

In `firecrab-frontend/src/bindings/index.ts`, add alongside the existing bootstrap exports:

```ts
export * from "./BootstrapStep";
export * from "./BootstrapStepOutcome";
export * from "./BootstrapStepRun";
```

- [ ] **Step 7: Verify the frontend still compiles**

Run: `cd firecrab-frontend && npm run build`
Expected: PASS. (`Images.tsx` doesn't read the new fields yet, so nothing else changes.)

- [ ] **Step 8: Commit**

```bash
git add firecrab-api-types/src/lib.rs firecrab-api/src/bootstrap.rs firecrab-frontend/src/bindings/
git commit -m "feat: add bootstrap step timeline wire types"
```

---

### Task 2: Record steps in BootstrapTracker

**Files:**
- Modify: `firecrab-api/src/bootstrap.rs`
- Test: `firecrab-api/src/bootstrap.rs` (its existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `BootstrapStep`, `BootstrapStepOutcome`, `BootstrapStepRun` from Task 1.
- Produces:
  - `BootstrapTracker::set_step(&self, id: Uuid, step: BootstrapStep)` — closes whatever step is open as `Succeeded`, then opens `step`. No-op if `id` is unknown.
  - `insert_session` now opens `BootstrapStep::StartingBuilderVm` as part of creating the session.
  - `finish_ok` / `finish_err` / `finish_err_from` close the open step (`Succeeded` for ok, `Failed` + the same reason string for the error paths) and clear `current_step`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `firecrab-api/src/bootstrap.rs`:

```rust
#[test]
fn a_new_session_opens_on_the_builder_vm_step() {
    let tracker = BootstrapTracker::default();
    let id = tracker
        .try_begin("alpine-3.24", "__microboot", Uuid::new_v4())
        .expect("first session");
    let session = tracker.get(id).expect("session");

    assert_eq!(session.current_step, Some(BootstrapStep::StartingBuilderVm));
    assert_eq!(session.step_timeline.len(), 1);
    assert_eq!(session.step_timeline[0].outcome, BootstrapStepOutcome::Running);
    assert_eq!(session.step_timeline[0].ended_at_ms, None);
}

#[test]
fn set_step_closes_the_previous_step_as_succeeded() {
    let tracker = BootstrapTracker::default();
    let id = tracker
        .try_begin("alpine-3.24", "__microboot", Uuid::new_v4())
        .expect("first session");

    tracker.set_step(id, BootstrapStep::InstallingSystem);
    let session = tracker.get(id).expect("session");

    assert_eq!(session.step_timeline.len(), 2);
    assert_eq!(
        session.step_timeline[0].outcome,
        BootstrapStepOutcome::Succeeded
    );
    assert!(session.step_timeline[0].ended_at_ms.is_some());
    assert_eq!(session.current_step, Some(BootstrapStep::InstallingSystem));
    assert_eq!(
        session.step_timeline[1].outcome,
        BootstrapStepOutcome::Running
    );
}

#[test]
fn finishing_ok_closes_the_last_step_and_clears_the_current_one() {
    let tracker = BootstrapTracker::default();
    let id = tracker
        .try_begin("alpine-3.24", "__microboot", Uuid::new_v4())
        .expect("first session");
    tracker.set_step(id, BootstrapStep::Finalizing);

    tracker.finish_ok(id);
    let session = tracker.get(id).expect("session");

    assert_eq!(session.current_step, None);
    assert!(
        session
            .step_timeline
            .iter()
            .all(|run| run.outcome == BootstrapStepOutcome::Succeeded),
        "every step should be succeeded: {:?}",
        session.step_timeline
    );
}

#[test]
fn failing_marks_the_step_that_was_open_and_carries_the_reason() {
    let tracker = BootstrapTracker::default();
    let id = tracker
        .try_begin("rocky-9", "__microboot", Uuid::new_v4())
        .expect("first session");
    tracker.set_step(id, BootstrapStep::InstallingSystem);

    tracker.finish_err(id, "bootstrap script exited with code 1");
    let session = tracker.get(id).expect("session");

    assert_eq!(session.current_step, None);
    let failed = session
        .step_timeline
        .iter()
        .find(|run| run.outcome == BootstrapStepOutcome::Failed)
        .expect("a failed step");
    assert_eq!(failed.step, BootstrapStep::InstallingSystem);
    assert_eq!(
        failed.detail.as_deref(),
        Some("bootstrap script exited with code 1")
    );
    // The earlier step still counts as done, not failed.
    assert_eq!(
        session.step_timeline[0].outcome,
        BootstrapStepOutcome::Succeeded
    );
}

#[test]
fn a_compare_and_set_failure_that_does_not_apply_leaves_the_timeline_alone() {
    let tracker = BootstrapTracker::default();
    let id = tracker
        .try_begin("alpine-3.24", "__microboot", Uuid::new_v4())
        .expect("first session");

    // Session is in `Booting`; this expects `Packaging`, so it must no-op.
    let applied = tracker.finish_err_from(id, BootstrapStatus::Packaging, "stale watcher");
    assert!(!applied);

    let session = tracker.get(id).expect("session");
    assert_eq!(session.current_step, Some(BootstrapStep::StartingBuilderVm));
    assert_eq!(
        session.step_timeline[0].outcome,
        BootstrapStepOutcome::Running
    );
}
```

Add the import at the top of `mod tests` (or extend the existing `use super::*;` with an explicit import if the types aren't re-exported):

```rust
use firecrab_api_types::{BootstrapStep, BootstrapStepOutcome};
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p firecrab-api bootstrap::tests`
Expected: FAIL — `no method named set_step`.

- [ ] **Step 3: Implement the step recording**

Change the `use` line at the top of `firecrab-api/src/bootstrap.rs`:

```rust
use firecrab_api_types::{
    BootstrapResponse, BootstrapStatus, BootstrapStep, BootstrapStepOutcome, BootstrapStepRun,
};
```

Add these two free functions next to `is_active`:

```rust
/// Closes whichever step is still open, if any. Idempotent, so both the
/// success and failure paths can call it unconditionally — same shape as
/// `handlers::vms::close_open_step`, which does this for VM startup.
fn close_open_step(
    session: &mut BootstrapResponse,
    now: u64,
    outcome: BootstrapStepOutcome,
    detail: Option<&str>,
) {
    if let Some(run) = session
        .step_timeline
        .iter_mut()
        .find(|run| run.outcome == BootstrapStepOutcome::Running)
    {
        run.ended_at_ms = Some(now);
        run.outcome = outcome;
        run.detail = detail.map(str::to_owned);
    }
    session.current_step = None;
}

/// Closes the open step as succeeded and opens `step` in its place.
fn open_step(session: &mut BootstrapResponse, now: u64, step: BootstrapStep) {
    close_open_step(session, now, BootstrapStepOutcome::Succeeded, None);
    session.step_timeline.push(BootstrapStepRun {
        step,
        started_at_ms: now,
        ended_at_ms: None,
        outcome: BootstrapStepOutcome::Running,
        detail: None,
    });
    session.current_step = Some(step);
}
```

Add this method to `impl BootstrapTracker`, right after `set_status_from`:

```rust
    /// Advances the session's step timeline: closes whatever step was open
    /// as succeeded, then opens `step`. Unlike `set_status_from` this is
    /// unconditional — every call site sits immediately after the status
    /// transition it accompanies, so the compare-and-set has already
    /// decided whether this session is the one still moving.
    pub fn set_step(&self, id: Uuid, step: BootstrapStep) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(session) = sessions.get_mut(&id) {
            open_step(session, now_ms(), step);
        }
    }
```

In `insert_session`, replace the two placeholder fields from Task 1 so the session opens on its first step. Change the literal's fields to:

```rust
            current_step: Some(BootstrapStep::StartingBuilderVm),
            step_timeline: vec![BootstrapStepRun {
                step: BootstrapStep::StartingBuilderVm,
                started_at_ms: now,
                ended_at_ms: None,
                outcome: BootstrapStepOutcome::Running,
                detail: None,
            }],
```

In `finish_ok`, add before setting `ended_at_ms`:

```rust
            close_open_step(session, now_ms(), BootstrapStepOutcome::Succeeded, None);
```

In `finish_err`, add inside the `if let Some(session)` block, before `session.ended_at_ms = ...`:

```rust
            close_open_step(
                session,
                now_ms(),
                BootstrapStepOutcome::Failed,
                Some(reason.as_ref()),
            );
```

In `finish_err_from`, add the identical `close_open_step` call inside the matching arm (the one that already sets `status = Failed`), before `session.ended_at_ms = ...`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p firecrab-api bootstrap::tests`
Expected: PASS — all five new tests plus the pre-existing ones.

- [ ] **Step 5: Verify nothing else regressed**

Run: `cargo fmt --all && cargo clippy -p firecrab-api --all-targets && cargo test -p firecrab-api`
Expected: PASS, no new clippy warnings.

- [ ] **Step 6: Commit**

```bash
git add firecrab-api/src/bootstrap.rs
git commit -m "feat: record a step timeline on bootstrap sessions"
```

---

### Task 3: Session-relative log timestamps

**Files:**
- Modify: `firecrab-api/src/bootstrap.rs`
- Test: `firecrab-api/src/bootstrap.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `clock` becomes `fn elapsed_label(started_at_ms: u64, now_ms: u64) -> String`, returning `"+42s"` style. Every log-appending method formats `[{elapsed_label}] {line}`.

**Why:** `clock()` currently prints `epoch_ms / 1000` — an absolute epoch second, rendered as `[1785900123s]`, which tells a reader nothing. Every log line in a session is relative to that session anyway.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn log_lines_are_stamped_relative_to_the_session_start() {
    assert_eq!(elapsed_label(1_000_000, 1_000_000), "+0s");
    assert_eq!(elapsed_label(1_000_000, 43_000_000), "+42s");
    // A clock that jumps backwards must not underflow into a huge number.
    assert_eq!(elapsed_label(43_000_000, 1_000_000), "+0s");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p firecrab-api log_lines_are_stamped`
Expected: FAIL — `cannot find function elapsed_label`.

- [ ] **Step 3: Replace `clock`**

Delete `fn clock(epoch_ms: u64) -> String` and add:

```rust
/// Log-line stamp, relative to when this session started. The absolute
/// epoch second this used to print (`[1785900123s]`) carried no usable
/// information for a reader scanning a single session's log.
/// `saturating_sub` because these are wall-clock reads, not monotonic ones,
/// and an NTP step backwards must not wrap into a nonsense duration.
fn elapsed_label(started_at_ms: u64, now_ms: u64) -> String {
    format!("+{}s", now_ms.saturating_sub(started_at_ms) / 1000)
}
```

Update all four call sites. Each currently reads `format!("[{}] {}", clock(now_ms()), ...)` and each has the session in scope, so it becomes:

```rust
format!("[{}] {}", elapsed_label(session.started_at_ms, now_ms()), ...)
```

In `insert_session` the session doesn't exist yet — use the local `now` for both arguments so the opening line reads `[+0s]`:

```rust
log: format!("[{}] builder VM starting", elapsed_label(now, now)),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p firecrab-api bootstrap`
Expected: PASS. If a pre-existing test asserts on the old `[NNNNs]` shape, update its expectation to the new relative form — that is the point of this task.

- [ ] **Step 5: Verify the workspace is clean**

Run: `cargo fmt --all && cargo clippy -p firecrab-api --all-targets && cargo test -p firecrab-api`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add firecrab-api/src/bootstrap.rs
git commit -m "fix: stamp bootstrap log lines with session-relative elapsed time"
```

---

### Task 4: Thread the steps through the bootstrap pipeline

**Files:**
- Modify: `firecrab-api/src/handlers/bootstrap.rs`
- Test: `firecrab-api/src/handlers/bootstrap.rs`

**Interfaces:**
- Consumes: `BootstrapTracker::set_step` from Task 2.
- Produces: no new API. Three `set_step` calls at the transitions the design doc names.

**The three call sites** (the fourth, `StartingBuilderVm`, is already opened by `insert_session`):

| Step | Where | Anchor in the current file |
|---|---|---|
| `InstallingSystem` | `run_bootstrap_script`, right after `wait_for_console_shell` returns `Ok` | immediately before `let heredoc = format!(` |
| `Packaging` | `run_bootstrap_script`'s `Ok((0, tail))` arm, inside the `if state.bootstraps.set_status_from(...)` block | immediately before `let state_for_package = state.clone();` |
| `Finalizing` | `package_bootstrap`, after `package_bootstrap_inner` succeeds | immediately before the `if super::vms::delete_vm(` call |

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `firecrab-api/src/handlers/bootstrap.rs`. This extends the existing console-script test, which already drives a fake console through to the packaging handoff:

```rust
#[tokio::test]
async fn running_the_script_advances_the_step_timeline_to_installing_system() {
    let directory = tempdir().unwrap();
    let state = test_state(directory.path()).await;
    let vm = seed_builder_vm(&state, VmState::Running);
    let console = register_fake_process(&state, vm.id);
    let bootstrap_id = seeded_session(
        &state,
        "ubuntu-26.04",
        "alpine-3.24",
        vm.id,
        BootstrapStatus::Running,
    );

    let state_for_script = state.clone();
    let handle = tokio::spawn(async move {
        run_bootstrap_script(&state_for_script, bootstrap_id, vm.id).await;
    });

    // Answer the shell probe so the script push proceeds.
    console.emit_console(format!("{CONSOLE_PROBE_SENTINEL}\n").as_bytes());

    // Poll until the step opens rather than sleeping a fixed amount — the
    // handler advances it on its own task.
    let mut opened = None;
    for _ in 0..100 {
        if let Some(session) = state.bootstraps.get(bootstrap_id) {
            if session.current_step == Some(BootstrapStep::InstallingSystem) {
                opened = Some(session);
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    handle.abort();

    let session = opened.expect("InstallingSystem should have opened");
    assert_eq!(session.step_timeline.len(), 2);
    assert_eq!(
        session.step_timeline[0].step,
        BootstrapStep::StartingBuilderVm
    );
    assert_eq!(
        session.step_timeline[0].outcome,
        BootstrapStepOutcome::Succeeded
    );
}
```

**Before writing this test, read the existing test `run_bootstrap_script_records_the_console_output_and_reaches_running_terminal_wait` in the same module** and match its exact fixture helpers (`test_state`, `seed_builder_vm`, `register_fake_process`, `seeded_session`, and however it emits console bytes — the method name above is a placeholder for whatever that test actually calls). Use the real helper names; do not invent them.

Add to the test module's imports:

```rust
use firecrab_api_types::{BootstrapStep, BootstrapStepOutcome};
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p firecrab-api running_the_script_advances`
Expected: FAIL — the timeline stays at one entry, `current_step` is still `StartingBuilderVm`.

- [ ] **Step 3: Add the three `set_step` calls**

In `run_bootstrap_script`, after the `wait_for_console_shell` error branch and before the heredoc:

```rust
    // The builder is up and answering; everything from here until the
    // sentinel is the guest script doing the actual install.
    state
        .bootstraps
        .set_step(bootstrap_id, BootstrapStep::InstallingSystem);
```

In the `Ok((0, tail))` arm, inside the `if state.bootstraps.set_status_from(...)` block, as its first statement:

```rust
                state
                    .bootstraps
                    .set_step(bootstrap_id, BootstrapStep::Packaging);
```

In `package_bootstrap`, after the `package_bootstrap_inner` error branch returns and before the `delete_vm` call:

```rust
    // Package is staged; the session's remaining work is teardown.
    state
        .bootstraps
        .set_step(bootstrap_id, BootstrapStep::Finalizing);
```

Add `BootstrapStep` to the file's existing `firecrab_api_types` import.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p firecrab-api bootstrap`
Expected: PASS.

- [ ] **Step 5: Verify the workspace is clean**

Run: `cargo fmt --all && cargo clippy -p firecrab-api --all-targets && cargo test -p firecrab-api`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add firecrab-api/src/handlers/bootstrap.rs
git commit -m "feat: advance the bootstrap step timeline at each phase transition"
```

---

### Task 5: English heartbeat log line

**Files:**
- Modify: `firecrab-api/src/handlers/bootstrap.rs` (`spawn_progress_heartbeat`, ~line 449)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing new.

**Why:** the only Korean string in the session log. Now that the live console carries real-time detail, this line's job shrinks to "a durable marker in the exported log that the run was still alive".

- [ ] **Step 1: Make the change**

In `spawn_progress_heartbeat`, replace the `append_log` argument:

```rust
                    state.bootstraps.append_log(
                        bootstrap_id,
                        format!(
                            "still running the install script ({}m elapsed)",
                            started.elapsed().as_secs() / 60
                        ),
                    );
```

Then update that function's doc comment — its current text ("the dashboard would look frozen for the whole multi-minute run") is no longer the reason this exists:

```rust
/// Appends a "still running" line to the session log every
/// [`BOOTSTRAP_HEARTBEAT_INTERVAL`] while the script executes. Live
/// progress is the inline console's job now; this line's remaining purpose
/// is the exported log, which is otherwise silent for the whole
/// multi-minute install and gives a later reader no way to tell a slow run
/// from a hung one. Runs on its own task so it never delays the sentinel
/// wait it accompanies — the caller aborts it the moment that wait returns.
```

- [ ] **Step 2: Verify**

Run: `cargo fmt --all && cargo clippy -p firecrab-api --all-targets && cargo test -p firecrab-api`
Expected: PASS. If a test asserts on the Korean string, update it.

- [ ] **Step 3: Confirm no Korean remains in the session log**

Run: `grep -nP '[\x{AC00}-\x{D7A3}]' firecrab-api/src/handlers/bootstrap.rs firecrab-api/src/bootstrap.rs`
Expected: no output.

- [ ] **Step 4: Commit**

```bash
git add firecrab-api/src/handlers/bootstrap.rs
git commit -m "refactor: write the bootstrap heartbeat line in english"
```

---

### Task 6: Inline live console component

**Files:**
- Create: `firecrab-frontend/src/components/InlineConsole.tsx`
- Modify: `firecrab-frontend/src/index.css`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `export default function InlineConsole({ vmId }: { vmId: string })` — a self-contained live terminal. The parent controls the connection purely by mounting/unmounting it.

**Read `firecrab-frontend/src/components/Console.tsx` first.** This component is its connection logic with all the page chrome removed: no toolbar, no settings popover, no VM detail panel, no terminal-only mode, no `LogExportActions`, no `getVm` polling. Keep the parts that are load-bearing and non-obvious — the shared terminal+socket effect (so a StrictMode remount can't leave a live socket writing into a disposed `Terminal`), the guarded `doFit` (a 0×0 container makes `fit()` set cols/rows to 0 and the terminal draws nothing), and the `consoleWsUrl` scheme/host derivation.

- [ ] **Step 1: Write the component**

```tsx
import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

type Status = "connecting" | "connected" | "reconnecting" | "disconnected";

const STATUS_LABEL: Record<Status, string> = {
  connecting: "연결 중…",
  connected: "실시간",
  reconnecting: "재연결 중…",
  disconnected: "연결 끊김",
};

const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 15000;

/**
 * Read-only live view of one VM's serial console, sized to sit inside a
 * panel rather than fill a page.
 *
 * This is `Console.tsx`'s connection logic with the page chrome removed —
 * see that component for the full-featured version. Two differences beyond
 * layout: input is never forwarded (the bootstrap script owns this console;
 * a stray keystroke would corrupt the heredoc it is being fed), and there
 * is no VM metadata polling, because the caller already knows which VM this
 * is and unmounts us the moment it stops existing.
 */
export default function InlineConsole({ vmId }: { vmId: string }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const socketRef = useRef<WebSocket | null>(null);
  const intentionalCloseRef = useRef(false);
  const reconnectAttemptRef = useRef(0);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const [status, setStatus] = useState<Status>("connecting");

  const clearReconnectTimer = useCallback(() => {
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
  }, []);

  /**
   * Fit only when the surface has a real box. Calling FitAddon.fit() with a
   * 0×0 container (first paint / StrictMode remount) sets cols/rows to 0 and
   * the terminal draws nothing until a later successful fit.
   */
  const doFit = useCallback(() => {
    const fit = fitRef.current;
    const term = termRef.current;
    const el = containerRef.current;
    if (!fit || !term || !el) return false;
    if (el.clientWidth < 16 || el.clientHeight < 16) return false;
    try {
      const proposed = fit.proposeDimensions();
      if (!proposed || proposed.cols < 2 || proposed.rows < 2) return false;
      fit.fit();
      return term.cols >= 2 && term.rows >= 2;
    } catch {
      return false;
    }
  }, []);

  const scheduleFit = useCallback(() => {
    let tries = 0;
    const tick = () => {
      if (doFit()) return;
      tries += 1;
      if (tries < 20) requestAnimationFrame(tick);
    };
    requestAnimationFrame(() => requestAnimationFrame(tick));
  }, [doFit]);

  // Terminal and socket share one effect so a StrictMode remount can never
  // leave a live socket writing into a disposed Terminal (blank screen).
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let disposed = false;
    intentionalCloseRef.current = false;
    reconnectAttemptRef.current = 0;

    const term = new Terminal({
      convertEol: true,
      fontFamily: '"IBM Plex Mono", ui-monospace, monospace',
      fontSize: 12,
      theme: {
        background: "#171b22",
        foreground: "#e8ecf1",
        cursor: "#c43e12",
        selectionBackground: "rgba(196, 62, 18, 0.35)",
      },
      scrollback: 5000,
      disableStdin: true,
      cursorBlink: false,
      cols: 80,
      rows: 16,
    });
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(container);
    termRef.current = term;
    fitRef.current = fitAddon;
    scheduleFit();

    const observer = new ResizeObserver(() => {
      if (!disposed) scheduleFit();
    });
    observer.observe(container);
    const onWinResize = () => {
      if (!disposed) scheduleFit();
    };
    window.addEventListener("resize", onWinResize);

    const scheduleReconnect = () => {
      if (disposed || intentionalCloseRef.current) return;
      clearReconnectTimer();
      const attempt = reconnectAttemptRef.current;
      const delay = Math.min(RECONNECT_BASE_MS * 2 ** attempt, RECONNECT_MAX_MS);
      reconnectAttemptRef.current = attempt + 1;
      setStatus("reconnecting");
      reconnectTimerRef.current = setTimeout(() => connect(true), delay);
    };

    const connect = (isRetry: boolean) => {
      if (disposed || intentionalCloseRef.current) return;

      const previous = socketRef.current;
      if (previous) {
        socketRef.current = null;
        previous.onopen = null;
        previous.onmessage = null;
        previous.onclose = null;
        try {
          previous.close();
        } catch {
          /* ignore */
        }
      }

      setStatus(isRetry ? "reconnecting" : "connecting");
      const socket = new WebSocket(consoleWsUrl(vmId));
      socket.binaryType = "arraybuffer";
      socketRef.current = socket;

      socket.onopen = () => {
        if (disposed || socketRef.current !== socket) return;
        reconnectAttemptRef.current = 0;
        setStatus("connected");
        scheduleFit();
      };

      socket.onmessage = (event: MessageEvent<ArrayBuffer | string>) => {
        if (disposed || socketRef.current !== socket) return;
        const live = termRef.current;
        if (!live) return;
        live.write(
          typeof event.data === "string" ? event.data : new Uint8Array(event.data),
        );
      };

      socket.onclose = () => {
        if (socketRef.current === socket) socketRef.current = null;
        if (disposed || intentionalCloseRef.current) return;
        setStatus("disconnected");
        scheduleReconnect();
      };
    };

    const bootTimer = window.setTimeout(() => {
      if (!disposed) connect(false);
    }, 0);

    return () => {
      disposed = true;
      intentionalCloseRef.current = true;
      window.clearTimeout(bootTimer);
      clearReconnectTimer();
      observer.disconnect();
      window.removeEventListener("resize", onWinResize);
      const socket = socketRef.current;
      if (socket) {
        socketRef.current = null;
        socket.onopen = null;
        socket.onmessage = null;
        socket.onclose = null;
        try {
          socket.close();
        } catch {
          /* ignore */
        }
      }
      term.dispose();
      if (termRef.current === term) termRef.current = null;
      if (fitRef.current === fitAddon) fitRef.current = null;
    };
  }, [vmId, clearReconnectTimer, scheduleFit]);

  return (
    <div className="inline-console">
      <div className="inline-console-bar">
        <span className="inline-console-title">빌더 VM 콘솔</span>
        <span className={`inline-console-status ${status}`} role="status" aria-live="polite">
          {STATUS_LABEL[status]}
        </span>
      </div>
      <div className="inline-console-surface" ref={containerRef} />
    </div>
  );
}

/**
 * Same derivation as `Console.tsx` — `/ws`, not `/api`, because REST and
 * WebSocket routes can't share a proxied path prefix (see the `/ws`
 * sub-router comment in `firecrab-api/src/server.rs`).
 */
function consoleWsUrl(vmId: string): string {
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/ws/vms/${vmId}/console`;
}
```

- [ ] **Step 2: Add the styles**

Append to `firecrab-frontend/src/index.css`. **Read the file's existing console styles first** (`.console-page`, `.console-surface`, `.console-bar`) and match its variable names and spacing conventions rather than hardcoding values it already has tokens for.

```css
/* Live builder-VM console embedded in the bootstrap panel. Fixed height:
   this sits inside a panel that must stay scannable, so the terminal gets a
   bounded window rather than growing with output. */
.inline-console {
  margin-top: 0.75rem;
  border: 1px solid var(--border);
  border-radius: 6px;
  overflow: hidden;
}

.inline-console-bar {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.35rem 0.6rem;
  background: var(--panel-alt, #1b2029);
  font-size: 0.8rem;
}

.inline-console-title {
  font-weight: 600;
}

.inline-console-status {
  margin-left: auto;
  font-variant-numeric: tabular-nums;
  opacity: 0.75;
}

.inline-console-status.connected {
  color: var(--ok, #3fb950);
  opacity: 1;
}

.inline-console-status.disconnected,
.inline-console-status.reconnecting {
  color: var(--warn, #d29922);
  opacity: 1;
}

.inline-console-surface {
  height: 16rem;
  padding: 0.4rem;
  background: #171b22;
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd firecrab-frontend && npm run build`
Expected: PASS. TypeScript will flag `InlineConsole` as unused only if the project has `noUnusedLocals` for exports — it is exported, so this should be clean.

- [ ] **Step 4: Commit**

```bash
git add firecrab-frontend/src/components/InlineConsole.tsx firecrab-frontend/src/index.css
git commit -m "feat(frontend): add an embeddable live console component"
```

---

### Task 7: Render the stepper and console in BootstrapPanel

**Files:**
- Modify: `firecrab-frontend/src/components/Images.tsx` (`BootstrapPanel`, ~lines 337-457)

**Interfaces:**
- Consumes: `BootstrapStep`, `BootstrapStepRun` (Task 1), `InlineConsole` (Task 6), and the `stepTimeline` / `currentStep` fields the server now sends (Tasks 2 and 4).
- Produces: nothing other tasks depend on.

**Read `PipelineStepper` in `firecrab-frontend/src/components/VmDetailModal.tsx` (~line 458) before writing `BootstrapStepper`.** The structure below is deliberately the same so the two screens read alike; reuse the `.pipeline` / `.pipeline-step` classes that already exist rather than adding new ones. `duration()` and `clockTime()` are local to `VmDetailModal` — copy them into `Images.tsx` only if that file doesn't already have equivalents (check first).

- [ ] **Step 1: Add the stepper component**

Add above `BootstrapPanel` in `Images.tsx`:

```tsx
const BOOTSTRAP_STEPS: BootstrapStep[] = [
  "startingBuilderVm",
  "installingSystem",
  "packaging",
  "finalizing",
];

const BOOTSTRAP_STEP_LABEL: Record<BootstrapStep, string> = {
  startingBuilderVm: "빌더 VM 준비",
  installingSystem: "시스템 설치",
  packaging: "패키징",
  finalizing: "마무리",
};

/**
 * Four-box progress view over one bootstrap session, mirroring
 * `VmDetailModal`'s `PipelineStepper` so a VM start and a bootstrap read the
 * same way. Durations come from the server's own timestamps — the 1s poll is
 * far too coarse to time the short steps — and only the open step ticks
 * locally between polls.
 */
function BootstrapStepper({ timeline }: { timeline: BootstrapStepRun[] }) {
  const [now, setNow] = useState(() => Date.now());
  const hasOpenStep = timeline.some((run) => run.outcome === "running");
  useEffect(() => {
    if (!hasOpenStep) return;
    const tick = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(tick);
  }, [hasOpenStep]);

  const runFor = (step: BootstrapStep) => timeline.find((run) => run.step === step);

  return (
    <ol className="pipeline">
      {BOOTSTRAP_STEPS.map((step) => {
        const run = runFor(step);
        const status = run ? run.outcome : "pending";
        const elapsed = run ? (run.endedAtMs ?? now) - run.startedAtMs : null;

        return (
          <li key={step} className={`pipeline-step ${status}`}>
            <span className="step-label">{BOOTSTRAP_STEP_LABEL[step]}</span>
            <span className="step-bar">
              <span className="step-time">
                {elapsed === null ? "—" : formatElapsed(elapsed)}
              </span>
              <span className="step-mark">
                {status === "succeeded" ? "✓" : status === "failed" ? "✕" : ""}
              </span>
            </span>
            {run?.detail && <span className="step-detail">{run.detail}</span>}
          </li>
        );
      })}
    </ol>
  );
}

/** Same shape as `VmDetailModal`'s `duration()`. */
function formatElapsed(millis: number): string {
  if (millis < 1000) return `${millis}ms`;
  const seconds = Math.round(millis / 1000);
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}
```

Note the deliberate difference from `PipelineStepper`: no `currentIndex` prop. The VM version needs it because `startupTimeline` is cleared on each restart and can lag `startupStep`; a bootstrap session's timeline is append-only for its whole life, so the runs alone say everything.

- [ ] **Step 2: Render both pieces**

Replace `BootstrapPanel`'s session block:

```tsx
      {session && (
        <>
          <div className="state-badge">{session.status}</div>
          <pre className="detail-log">{session.log}</pre>
        </>
      )}
```

with:

```tsx
      {session && (
        <>
          <div className="state-badge">{session.status}</div>
          <BootstrapStepper timeline={session.stepTimeline} />
          {/* The builder VM only exists while the session is pre-terminal:
              `packaging` is entered *after* stop_vm returns, and the VM is
              deleted at the end. Gate on status, never on vmId — that field
              keeps its value after the VM it names is gone. */}
          {(session.status === "booting" || session.status === "running") ? (
            <InlineConsole vmId={session.vmId} />
          ) : (
            <p className="inline-console-ended">
              빌더 VM이 정리되어 콘솔 연결이 종료되었습니다.
            </p>
          )}
          <pre className="detail-log">{session.log}</pre>
        </>
      )}
```

- [ ] **Step 3: Stop polling a session that no longer exists**

`pollBootstrap`'s `catch` currently reschedules on *any* failure, deliberately, so a one-off network blip doesn't freeze the log. But cancelling a bootstrap (`DELETE /api/images/bootstrap/{id}`) removes the tracker entry outright, so `getBootstrap` starts returning 404 forever and the panel polls a dead id for as long as it stays mounted — and with this task's changes it would also keep an `InlineConsole` mounted against a deleted VM.

Read how `firecrab-frontend/src/api/client.ts` surfaces a non-2xx (whether the thrown error carries a status code). If it does, narrow the catch; if it doesn't, add the status to the thrown error there first — do not string-match the message.

```tsx
      } catch (err) {
        // A cancelled session is deleted from the tracker, so 404 is a real
        // "this is over" answer, not a blip — stop and clear the panel.
        // Anything else is not positive confirmation of a terminal state, so
        // keep polling rather than freezing on one bad response.
        if ((err as { status?: number }).status === 404) {
          if (mountedRef.current) setSession(null);
          return;
        }
        if (mountedRef.current) setTimeout(() => void tick(), 1000);
      }
```

- [ ] **Step 4: Add the imports**

Extend the existing `../bindings` import with `BootstrapStep` and `BootstrapStepRun` (type-only, matching the file's existing convention), add `import InlineConsole from "./InlineConsole";`, and make sure `useEffect` is in the `react` import (`BootstrapStepper` uses it).

- [ ] **Step 5: Add the one new style**

Append to `firecrab-frontend/src/index.css`:

```css
.inline-console-ended {
  margin-top: 0.75rem;
  font-size: 0.85rem;
  opacity: 0.7;
}
```

- [ ] **Step 6: Verify it compiles and lints**

Run: `cd firecrab-frontend && npm run build && npm run lint`
Expected: PASS. (There is no frontend test runner in this workspace — no vitest, no jest — so `build` + `lint` is the whole automated gate for frontend code. Behavioural verification is Task 8's job.)

- [ ] **Step 7: Commit**

```bash
git add firecrab-frontend/src/components/Images.tsx firecrab-frontend/src/index.css
git commit -m "feat(frontend): show bootstrap stages and live console in the images panel"
```

---

### Task 8: Live end-to-end verification

**Files:** none — this task changes no code unless it finds a defect.

**Interfaces:** consumes everything.

This plan's whole point is what an operator sees during a real multi-minute run, and none of that is reachable from unit tests. Every prior plan on this branch that skipped a live pass shipped a bug that only a real boot exposed. There is also no frontend test runner in this workspace, so for Tasks 6 and 7 this is the *only* behavioural verification that exists.

**One alias, not three** — a deliberate narrowing of the spec's "3개 배포판 … 수동 검증" criterion. Every code path this plan adds is driven by `BootstrapStatus` and `stepTimeline`, neither of which varies by alias: the stepper renders the same four boxes and the console connects to the same endpoint regardless of what is being built. The MicroBoot plan's three-distro requirement was different in kind — there the *guest scripts* differed per distro, which is exactly where its bugs were. Spending an extra ~40 minutes of wall clock here would re-exercise identical frontend code. Use `alpine-3.24`, the fastest.

- [ ] **Step 1: Start the stack**

```bash
cargo build -p firecrab-api
# stop any running instance first, then:
target/debug/firecrab-api
# in a second terminal:
cd firecrab-frontend && npm run dev
```

- [ ] **Step 2: Free an alias to bootstrap**

The panel disables an alias that is already installed or already has a staged package. Check with `curl -s localhost:8080/api/images | python3 -m json.tool` and, if needed, `curl -X DELETE localhost:8080/api/images/<alias>` plus `rm -f images/.packages/<alias>.tar.zst`.

- [ ] **Step 3: Run a real bootstrap and watch it**

Click "alpine-3.24 부트스트랩" (the fastest of the three). Confirm, in order:

- The stepper shows four boxes; `빌더 VM 준비` is the only one running.
- The inline console connects (status reads `실시간`) and shows real guest output — kernel messages, then the script's `[INFO]` lines.
- `빌더 VM 준비` closes with `✓` and a plausible duration; `시스템 설치` opens.
- The console keeps streaming for the whole install.
- On `packaging`, the console is replaced by the "빌더 VM이 정리되어…" line — **not** a stuck "연결 중…" spinner, which would mean the status gate is wrong.
- `패키징` then `마무리` each open and close.
- The session log's timestamps read `[+Ns]`, and no Korean appears in it.

- [ ] **Step 4: Verify the failure rendering**

Start another bootstrap and cancel it partway (`curl -X DELETE localhost:8080/api/images/bootstrap/<id>`), or trigger a failure some other way, and confirm the step that was open renders `✕` with its reason in `.step-detail`, and that later steps stay `pending`.

- [ ] **Step 5: Record the outcome**

Write what you observed into the SDD ledger for this plan. If you found defects, fix them, re-verify, and commit each fix separately.

- [ ] **Step 6: Final gate**

```bash
cargo fmt --all -- --check
cargo clippy -p firecrab-api --all-targets
cargo test -p firecrab-api
cd firecrab-frontend && npm run build
```

Expected: all clean.

---

## Deliberately Out of Scope

- Per-stage sub-progress inside `installingSystem` (a percentage, a package counter). The live console is the answer to that; more enum variants are not.
- Persisting the timeline. `BootstrapTracker` is in-memory and stays that way.
- Recovering a bootstrap session across an API restart. Pre-existing gap, unchanged by this plan.
- Instrumenting the MicroBoot artifact download. It now runs at startup, before any session exists (`microboot::spawn_warmup`), so there is no session to attribute it to.
