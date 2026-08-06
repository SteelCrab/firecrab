# M2Image 웹 빌드 · 패키지 관리 · 화면 정리 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the web dashboard build M2Image templates itself (boot a microVM off an installed template, install/remove packages inside it, snapshot the result as a new or updated template) and manage packages on running VMs, without adding docker or any new privileged host daemon.

**Architecture:** Reuse the existing VM lifecycle (`create_vm`/`start_vm_request`/console/`delete_vm`) to run "builder" VMs tagged with a new `purpose` column so they never show up in the normal VM list. A generalized package engine (`install`/`remove`/`update`) drives both builder VMs and running instances over the console, reusing the sentinel-wait pattern already in `handlers/packages.rs`. Finishing a build stops the VM, copies its rootfs disk out from under `delete_vm`'s cleanup, strips machine identity, and registers it as a new `TemplateSpec` version — sharing the source template's kernel/initrd. The Images screen collapses from two panels (Store + Packer, with a fake 4-stage pipeline) into one table plus a build modal.

**Tech Stack:** Rust (axum, rusqlite, tokio), React 19 + TypeScript (no test framework configured), hand-maintained `firecrab-frontend/src/bindings/*.ts` mirrors of `firecrab-api-types`.

## Global Constraints

- `firecrab-api` stays a non-privileged process — no new root daemon, no docker dependency (`packaging/systemd/firecrab-api.service` keeps `NoNewPrivileges=yes` / `ProtectSystem=full`).
- Full from-scratch bootstrap of a brand-new distro family (never-before-built alias) stays on the CLI (`scripts/build-m2images.sh`) — out of scope. Web build only derives from an alias already installed via `TemplateRegistry`.
- Guest kernel/initrd are never changed by a web build — only rootfs (packages). New template versions share the source's `kernel`/`initrd` artifacts.
- Every package name reaching a guest console must pass `^[a-zA-Z0-9][a-zA-Z0-9._+-]*$`, max 32 packages per call — these strings are written directly into the VM's serial console.
- Rust: `rustfmt` + `clippy` clean, existing test patterns (`tokio::test`, `tempdir()`, `handlers::vms::test_support`) reused, not reinvented.
- Frontend: no test framework exists (`firecrab-frontend/package.json` has no test script) — frontend tasks are verified by `tsc -b` (via `npm run build`) and manual browser check, not automated tests.
- Follow existing doc comment style: explain *why*, not *what*; no comments restating the code.

---

## File Structure

**Backend (`firecrab-api/`, `firecrab-api-types/`):**
- `firecrab-api/src/model.rs` — add `VmPurpose` enum + `VmRecord.purpose` field
- `firecrab-api/src/persistence.rs` — add `purpose` column, migration, CRUD wiring
- `firecrab-api/src/handlers/vms.rs` — `list_vms` filters to `purpose == Instance`; `create_vm`/test `record()` set `purpose`
- `firecrab-api/src/handlers/packages.rs` — generalize `update_packages` → `run_package_action` (`install`/`remove`/`update`)
- `firecrab-api-types/src/lib.rs` — `PackageAction` request type, `BuildStatus`/`BuildResponse` types
- `firecrab-api/src/rootfs.rs` — `finalize_template_disk` (e2fsck + identity strip for a *template*, distinct from per-instance `specialize_guest`)
- `firecrab-api/src/builds.rs` — new: `BuildTracker` (mirrors `ImageInstallTracker`)
- `firecrab-api/src/handlers/builds.rs` — new: build session HTTP handlers
- `firecrab-api/src/server.rs` — route wiring

**Frontend (`firecrab-frontend/src/`):**
- `bindings/PackageAction.ts`, `bindings/BuildStatus.ts`, `bindings/BuildResponse.ts` — new hand-written mirrors + `bindings/index.ts` export
- `api/client.ts` — package action + build session client functions
- `components/VmDetailModal.tsx` — package install/remove/update section
- `components/Images.tsx` — full rewrite: single table + build modal (replaces the Store/Packer two-panel layout)
- `index.css` — remove dead `.packer-*` rules only used by the removed pipeline UI, add minimal rules for the new build modal if the existing `panel`/`btn`/`vm-table` classes don't cover it

**Docs:**
- `docs/20-guides/m2image-builder.md` — add web build section
- `docs/30-tasks/task-m2image-builder.md` — note web build as an additional entry point

---

### Task 1: `vms.purpose` column — builder VMs hidden from the normal list

**Files:**
- Modify: `firecrab-api/src/model.rs`
- Modify: `firecrab-api/src/persistence.rs`
- Modify: `firecrab-api/src/handlers/vms.rs` (`create_vm` at line ~158, `list_vms` at line ~27, `test_support::record` at line ~1675)
- Modify: `firecrab-api/src/firecracker.rs` (test `record()` at line ~701)
- Test: inline `#[cfg(test)]` in `persistence.rs` and `handlers/vms.rs`

**Interfaces:**
- Produces: `firecrab_api::model::VmPurpose` (`Instance` | `Builder`), `VmRecord.purpose: VmPurpose`, `Store::insert`/`update`/`load_all` round-trip it, `list_vms` only returns `Instance` records.

- [ ] **Step 1: Add `VmPurpose` to `model.rs`**

In `firecrab-api/src/model.rs`, after the `Lease` struct (before `VmRecord`):

```rust
/// What a VM record represents. Only `Builder` VMs are hidden from the
/// dashboard's normal list — everything else about their lifecycle (start,
/// console, stop, delete) is identical to a user-created instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VmPurpose {
    /// A user-created VM, shown in the normal MicroVM list.
    #[default]
    Instance,
    /// A short-lived VM driving an image build (`handlers::builds`) — never
    /// shown in `list_vms`, only in `GET /api/images/builds`.
    Builder,
}

impl VmPurpose {
    pub fn id(self) -> &'static str {
        match self {
            VmPurpose::Instance => "instance",
            VmPurpose::Builder => "builder",
        }
    }
}
```

Add `pub purpose: VmPurpose,` to `VmRecord` right after the `id`/`name` fields (a doc comment: `/// What this record represents — see [VmPurpose].`).

- [ ] **Step 2: Update every `VmRecord` literal to compile again**

Four sites need `purpose: VmPurpose::Instance,` (or `Default::default()`) added:
- `firecrab-api/src/handlers/vms.rs:158` (inside `create_vm`, in the `VmRecord { ... }` literal)
- `firecrab-api/src/handlers/vms.rs:1675` (`test_support::record`)
- `firecrab-api/src/firecracker.rs:701` (test `record()`)
- `firecrab-api/src/persistence.rs:883` (test `record()`)

(`persistence.rs:425`'s `load_all` construction is handled in Step 4 below — it reads the column instead of hardcoding.)

Run `cargo check -p firecrab-api` after this step — it will still fail on `persistence.rs` (missing column handling), which is expected; confirms every *other* construction site is fixed.

- [ ] **Step 3: Add the column + migration in `persistence.rs`**

Add to `CREATE_TABLE_SQL` (before the closing `) STRICT"`):
```sql
    purpose TEXT NOT NULL DEFAULT 'instance'
```
(append after `last_runtime_id TEXT`, with a comma).

Add a migration function next to `migrate_disk_generation_columns` (same file):
```rust
/// Adds `purpose` to a `vms` table created before it existed. `'instance'`
/// matches the only kind of VM that could exist before builder VMs did.
fn migrate_purpose_column(conn: &Connection) -> Result<(), PersistenceError> {
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('vms') WHERE name = 'purpose'")?
        .exists([])?;
    if !has_column {
        conn.execute(
            "ALTER TABLE vms ADD COLUMN purpose TEXT NOT NULL DEFAULT 'instance'",
            [],
        )?;
    }
    Ok(())
}
```

Call it in `Store::open`, right after `migrate_disk_generation_columns(&conn)?;`:
```rust
        migrate_purpose_column(&conn)?;
```

- [ ] **Step 4: Wire `purpose` through `SELECT`/`INSERT`/`UPDATE`/`IMPORT`/`load_all`/`execute_record`**

Update the five SQL constants (append `purpose` as the last column, `?17` where applicable):
```rust
const SELECT_ALL_SQL: &str = "SELECT id, name, state, template, template_version, \
    template_kernel_sha256, template_rootfs_sha256, template_boot_args_sha256, cpu, ram, disk_gb, \
    egress_policy, micro_network_id, storage_root, disk_generation, last_runtime_id, purpose FROM vms";

const INSERT_SQL: &str = "INSERT INTO vms (id, name, state, template, template_version, \
    template_kernel_sha256, template_rootfs_sha256, template_boot_args_sha256, cpu, ram, disk_gb, \
    egress_policy, micro_network_id, storage_root, disk_generation, last_runtime_id, purpose) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)";

const IMPORT_SQL: &str = "INSERT OR REPLACE INTO vms (id, name, state, template, \
    template_version, template_kernel_sha256, template_rootfs_sha256, \
    template_boot_args_sha256, cpu, ram, disk_gb, egress_policy, micro_network_id, storage_root, \
    disk_generation, last_runtime_id, purpose) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)";

const UPDATE_SQL: &str = "UPDATE vms SET name = ?2, state = ?3, template = ?4, \
    template_version = ?5, template_kernel_sha256 = ?6, template_rootfs_sha256 = ?7, \
    template_boot_args_sha256 = ?8, cpu = ?9, ram = ?10, disk_gb = ?11, egress_policy = ?12, \
    micro_network_id = ?13, storage_root = ?14, disk_generation = ?15, last_runtime_id = ?16, \
    purpose = ?17 WHERE id = ?1";
```

Add a decode/encode pair next to `decode_egress_policy`:
```rust
fn decode_purpose(id: &str, purpose: &str) -> Result<crate::model::VmPurpose, PersistenceError> {
    match purpose {
        "instance" => Ok(crate::model::VmPurpose::Instance),
        "builder" => Ok(crate::model::VmPurpose::Builder),
        other => Err(PersistenceError::CorruptRecord {
            id: id.to_owned(),
            reason: format!("unknown purpose {other:?}"),
        }),
    }
}
```

In `load_all`'s `VmRecord { ... }` literal (`persistence.rs:425`), add:
```rust
                    purpose: decode_purpose(&id_text, &row.get::<_, String>(16)?)?,
```
(replacing the implicit "no such field" gap).

In `execute_record` (used by insert/update/import), add `vm.purpose.id(),` as the last entry in the `params![...]` list.

- [ ] **Step 5: Write the round-trip test**

In `persistence.rs`'s `#[cfg(test)] mod tests`, add:
```rust
#[test]
fn purpose_round_trips_through_insert_and_load() {
    let dir = tempdir().unwrap();
    let store = Store::open(&dir.path().join("test.db")).unwrap();
    let mut vm = record(Uuid::new_v4(), "builder-vm");
    vm.purpose = crate::model::VmPurpose::Builder;
    store.insert(&vm).unwrap();

    let loaded = store.load_all().unwrap();
    assert_eq!(loaded[&vm.id].purpose, crate::model::VmPurpose::Builder);
}
```
This needs `VmPurpose: PartialEq` — already derived in Step 1.

- [ ] **Step 6: Run it, verify it fails then passes**

Run: `cargo test -p firecrab-api purpose_round_trips_through_insert_and_load -- --nocapture`
Before Step 4 this fails to compile; after Steps 1–4 it should pass.

- [ ] **Step 7: Filter `list_vms` to `Instance` only**

In `firecrab-api/src/handlers/vms.rs`, `list_vms` (line ~27) currently does something like collect all `state.vms` values into `VmResponse`s. Add a filter before mapping:
```rust
pub async fn list_vms(State(state): State<AppState>) -> Json<Vec<VmResponse>> {
    let vms = state.vms.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut responses: Vec<VmResponse> = vms
        .values()
        .filter(|vm| vm.purpose == crate::model::VmPurpose::Instance)
        .map(|vm| vm_response(vm, /* existing lease lookup call */))
        .collect();
    // ...unchanged sort/return
}
```
(Match this against the actual current body — only add the `.filter(...)` line; don't restructure lease lookup or sorting.)

- [ ] **Step 8: Test the filter**

Add to `handlers/vms.rs`'s test module:
```rust
#[tokio::test]
async fn list_vms_excludes_builder_purpose_records() {
    let directory = tempdir().unwrap();
    let state = test_support::test_state(directory.path()).await;
    let mut builder_vm = test_support::record("hidden-builder", Uuid::new_v4());
    builder_vm.purpose = crate::model::VmPurpose::Builder;
    test_support::seed_vm(&state, &builder_vm);
    let instance_vm = test_support::record("visible-instance", Uuid::new_v4());
    test_support::seed_vm(&state, &instance_vm);

    let Json(listed) = list_vms(State(state)).await;

    assert!(listed.iter().any(|vm| vm.id == instance_vm.id));
    assert!(!listed.iter().any(|vm| vm.id == builder_vm.id));
}
```

Run: `cargo test -p firecrab-api list_vms_excludes_builder_purpose_records`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
git add firecrab-api/src/model.rs firecrab-api/src/persistence.rs firecrab-api/src/handlers/vms.rs firecrab-api/src/firecracker.rs
git commit -m "feat: add vms.purpose column, hide builder VMs from list_vms"
```

---

### Task 2: Generalize the package engine (install / remove / update)

**Files:**
- Modify: `firecrab-api-types/src/lib.rs` (new `PackageAction` request type)
- Create: `firecrab-frontend/src/bindings/PackageAction.ts`
- Modify: `firecrab-frontend/src/bindings/index.ts`
- Modify: `firecrab-api/src/handlers/packages.rs`
- Modify: `firecrab-api/src/server.rs` (route: `/api/vms/{id}/packages/update` → `/api/vms/{id}/packages`)

**Interfaces:**
- Consumes: `PackageManager::for_template(&str) -> Option<Self>` (existing), `VmProcess.console` (existing `ConsoleBroker`), `wait_for_completion` (existing).
- Produces: `firecrab_api_types::PackageAction { action: PackageActionKind, packages: Vec<String> }`, `pub async fn run_package_action(State, Extension<RequestId>, Path<String>, Json<PackageAction>) -> Result<Json<VmResponse>, AppError>` (replaces `update_packages` as the route handler; `update_packages`'s internals become the `Update` branch of the same function).

- [ ] **Step 1: Add the wire type in `firecrab-api-types/src/lib.rs`**

Place near `PackageUpdateStatus` (reuse that response type unchanged — only the request shape is new):
```rust
/// What `POST /api/vms/{id}/packages` should do on the guest's console.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackageActionKind {
    Install,
    Remove,
    Update,
}

/// Body of `POST /api/vms/{id}/packages`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PackageAction {
    pub action: PackageActionKind,
    /// Required (non-empty) for `install`/`remove`; ignored for `update`.
    #[serde(default)]
    pub packages: Vec<String>,
}
```

- [ ] **Step 2: Write the failing test for package name validation**

In `firecrab-api/src/handlers/packages.rs`, add near the existing tests:
```rust
#[test]
fn validate_package_names_rejects_shell_metacharacters() {
    let error = validate_package_names(&["nginx; rm -rf /".to_owned()]).unwrap_err();
    assert!(error.contains("invalid package name"));
}

#[test]
fn validate_package_names_rejects_more_than_the_cap() {
    let packages: Vec<String> = (0..33).map(|n| format!("pkg{n}")).collect();
    let error = validate_package_names(&packages).unwrap_err();
    assert!(error.contains("too many packages"));
}

#[test]
fn validate_package_names_accepts_ordinary_names() {
    assert!(validate_package_names(&["nginx".to_owned(), "postgresql-16".to_owned()]).is_ok());
}

#[test]
fn validate_package_names_rejects_empty_list() {
    let error = validate_package_names(&[]).unwrap_err();
    assert!(error.contains("at least one package"));
}
```

Run: `cargo test -p firecrab-api validate_package_names -- --nocapture`
Expected: FAIL (function doesn't exist yet).

- [ ] **Step 3: Implement `validate_package_names` and extend `PackageManager`**

Add near the top of `packages.rs`, after the `PackageManager` impl block:
```rust
const MAX_PACKAGES_PER_ACTION: usize = 32;

/// Guards every package name that reaches the guest's console verbatim —
/// the sentinel-wait command is built by string concatenation, so this is
/// the only thing standing between an arbitrary UI input and shell
/// injection into the VM's own console.
fn validate_package_names(packages: &[String]) -> Result<(), String> {
    if packages.is_empty() {
        return Err("at least one package is required".to_owned());
    }
    if packages.len() > MAX_PACKAGES_PER_ACTION {
        return Err(format!(
            "too many packages: {} (max {MAX_PACKAGES_PER_ACTION})",
            packages.len()
        ));
    }
    let is_valid_name = |name: &str| {
        let mut chars = name.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
    };
    match packages.iter().find(|name| !is_valid_name(name)) {
        Some(bad) => Err(format!("invalid package name: {bad:?}")),
        None => Ok(()),
    }
}
```

Extend `PackageManager` with an action-aware command builder (replace the existing `update_command` method with this, keeping call sites working via the `Update` branch):
```rust
impl PackageManager {
    // ...existing for_template unchanged...

    /// The shell command run on the guest's console for one package action,
    /// wrapped in a subshell so the final `$?` reflects the compound
    /// command's own exit status rather than just the trailing `echo`.
    fn command_for(self, action: PackageActionKind, packages: &[String]) -> String {
        let joined = packages.join(" ");
        match (self, action) {
            (Self::Apt, PackageActionKind::Update) => {
                "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get upgrade -y".to_owned()
            }
            (Self::Apt, PackageActionKind::Install) => {
                format!("DEBIAN_FRONTEND=noninteractive apt-get install -y {joined}")
            }
            (Self::Apt, PackageActionKind::Remove) => {
                format!("DEBIAN_FRONTEND=noninteractive apt-get remove -y {joined}")
            }
            (Self::Apk, PackageActionKind::Update) => "apk update && apk upgrade".to_owned(),
            (Self::Apk, PackageActionKind::Install) => format!("apk add {joined}"),
            (Self::Apk, PackageActionKind::Remove) => format!("apk del {joined}"),
            (Self::Dnf, PackageActionKind::Update) => "dnf -y upgrade --refresh".to_owned(),
            (Self::Dnf, PackageActionKind::Install) => format!("dnf -y install {joined}"),
            (Self::Dnf, PackageActionKind::Remove) => format!("dnf -y remove {joined}"),
        }
    }
}
```
Add `use firecrab_api_types::PackageActionKind;` to the imports at the top of the file.

Run: `cargo test -p firecrab-api validate_package_names -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Rename `update_packages` to `run_package_action`, dispatch on the body**

Replace the `update_packages` handler's signature and body start (keep everything from `let process = ...` onward structurally the same, just parameterize the command):
```rust
pub async fn run_package_action(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Json(body): Json<PackageAction>,
) -> Result<Json<VmResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;

    if body.action != PackageActionKind::Update {
        if let Err(reason) = validate_package_names(&body.packages) {
            let mut fields = BTreeMap::new();
            fields.insert("packages".to_owned(), reason);
            return Err(AppError::validation(fields, request_id.0));
        }
    }

    let template = {
        let vms = state.vms.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let vm = vms.get(&id).ok_or_else(|| AppError::not_found(request_id.0))?;
        if vm.state != VmState::Running {
            return Err(AppError::invalid_state(vm.state, request_id.0));
        }
        vm.template.clone()
    };

    let manager = PackageManager::for_template(&template).ok_or_else(|| {
        let mut fields = BTreeMap::new();
        fields.insert("template".to_owned(), "has no known package manager".to_owned());
        AppError::validation(fields, request_id.0)
    })?;

    let process = state
        .processes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&id)
        .cloned()
        .ok_or_else(|| AppError::vm_not_running(request_id.0))?;

    if let Some(vm) = state.vms.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get_mut(&id) {
        vm.package_update = Some(PackageUpdateStatus::Running);
    }

    let state_for_task = state.clone();
    let command = manager.command_for(body.action, &body.packages);
    tokio::spawn(async move {
        run_action(&state_for_task, id, process, command).await;
    });

    let vm = state.vms.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(&id).cloned()
        .ok_or_else(|| AppError::not_found(request_id.0))?;
    let lease = lease_for(&state, id).await;
    Ok(Json(vm_response(&vm, lease.as_ref())))
}
```

Rename the old `run_update(state, id, process, manager)` to take the already-built command string instead of `manager`:
```rust
async fn run_action(state: &AppState, id: Uuid, process: VmProcess, command: String) {
    let (_backlog, mut receiver) = process.console.subscribe();
    let full_command = format!("({command}); echo \"{DONE_SENTINEL}:$?\"\n");
    process.console.write_input(full_command.as_bytes()).await;
    // ...unchanged body from here (wait_for_completion, status recording)...
}
```
(Everything from `let status = match wait_for_completion(...)` to the end of the old `run_update` stays identical — only the signature and the `command` construction line change; the old two-line `format!("({}); ...", manager.update_command())` becomes `format!("({command}); ...")` above.)

Delete the old `update_command` references (already replaced by `command_for` in Step 3).

- [ ] **Step 5: Update existing tests to call the new signature**

The 4 existing `#[tokio::test]` functions in `packages.rs` call `update_packages(State(state), Extension(...), Path(...))`. Update each call site to:
```rust
run_package_action(
    State(state),
    Extension(RequestId(Uuid::new_v4())),
    Path(vm.id.to_string()),
    Json(PackageAction { action: PackageActionKind::Update, packages: Vec::new() }),
)
```
Add `use firecrab_api_types::{PackageAction, PackageActionKind};` to the test module's imports (or the top-level imports if not already covered).

Run: `cargo test -p firecrab-api handlers::packages:: -- --nocapture`
Expected: all existing + new tests PASS.

- [ ] **Step 6: Add install/remove-specific tests**

```rust
#[tokio::test]
async fn run_package_action_rejects_install_with_no_packages() {
    let directory = tempdir().unwrap();
    let state = test_state(directory.path()).await;
    let vm = VmRecord { state: VmState::Running, ..record("test-vm", Uuid::new_v4()) };
    seed_vm(&state, &vm);
    register_fake_process(&state, vm.id);

    let error = run_package_action(
        State(state),
        Extension(RequestId(Uuid::new_v4())),
        Path(vm.id.to_string()),
        Json(PackageAction { action: PackageActionKind::Install, packages: Vec::new() }),
    )
    .await
    .unwrap_err();

    assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
}

#[test]
fn command_for_maps_install_and_remove_per_distro() {
    let packages = vec!["nginx".to_owned()];
    assert_eq!(
        PackageManager::Apt.command_for(PackageActionKind::Install, &packages),
        "DEBIAN_FRONTEND=noninteractive apt-get install -y nginx"
    );
    assert_eq!(
        PackageManager::Apk.command_for(PackageActionKind::Remove, &packages),
        "apk del nginx"
    );
    assert_eq!(
        PackageManager::Dnf.command_for(PackageActionKind::Install, &packages),
        "dnf -y install nginx"
    );
}
```

Run: `cargo test -p firecrab-api handlers::packages:: -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Wire the route in `server.rs`**

Replace:
```rust
        .route(
            "/api/vms/{id}/packages/update",
            post(handlers::packages::update_packages),
        )
```
with:
```rust
        .route(
            "/api/vms/{id}/packages",
            post(handlers::packages::run_package_action),
        )
```

- [ ] **Step 8: Full check + commit**

```bash
cargo fmt -p firecrab-api -p firecrab-api-types
cargo clippy -p firecrab-api -p firecrab-api-types --all-targets -- -D warnings
cargo test -p firecrab-api -p firecrab-api-types
git add firecrab-api-types/src/lib.rs firecrab-api/src/handlers/packages.rs firecrab-api/src/server.rs
git commit -m "feat: generalize package endpoint to install/remove/update"
```

---

### Task 3: Frontend — package management in the VM detail modal

**Files:**
- Create: `firecrab-frontend/src/bindings/PackageAction.ts`
- Modify: `firecrab-frontend/src/bindings/index.ts`
- Modify: `firecrab-frontend/src/api/client.ts`
- Modify: `firecrab-frontend/src/components/VmDetailModal.tsx`

**Interfaces:**
- Consumes: `POST /api/vms/{id}/packages` from Task 2, existing `PackageUpdateStatus`/`VmResponse.packageUpdate` binding (already present per grep — if missing, add `packageUpdate?: PackageUpdateStatus` to `bindings/VmResponse.ts` mirroring the Rust field).
- Produces: `runPackageAction(id: string, action: "install"|"remove"|"update", packages?: string[]): Promise<VmResponse>` in `api/client.ts`.

- [ ] **Step 1: Add the binding**

Create `firecrab-frontend/src/bindings/PackageAction.ts`:
```ts
// Mirrors firecrab_api_types::{PackageAction, PackageActionKind} (camelCase wire shape).

export type PackageActionKind = "install" | "remove" | "update";

export type PackageAction = {
  action: PackageActionKind;
  packages: string[];
};
```
Add `export * from "./PackageAction";` to `firecrab-frontend/src/bindings/index.ts`.

Check `firecrab-frontend/src/bindings/VmResponse.ts` for a `packageUpdate` field. If absent, add `packageUpdate: PackageUpdateStatus | null,` to the type and confirm `PackageUpdateStatus` is exported from `bindings/index.ts` (it should already exist as a file, given `handlers/packages.rs` already returns it).

- [ ] **Step 2: Add the client function**

In `firecrab-frontend/src/api/client.ts`, near `updateVmResources`/`stopVm`:
```ts
/** Install, remove, or update packages on a running VM (`POST /api/vms/{id}/packages`). */
export function runPackageAction(
  id: string,
  action: PackageActionKind,
  packages: string[] = [],
): Promise<VmResponse> {
  return fetchJson(`/api/vms/${encodeURIComponent(id)}/packages`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ action, packages }),
  });
}
```
Add `PackageActionKind` to the existing `import type { ... } from "../bindings"` line at the top of the file.

- [ ] **Step 3: Add the UI section to `VmDetailModal.tsx`**

Add two pieces of state near the existing `saving`/`saveError` state (around line 80):
```tsx
const [installInput, setInstallInput] = useState("");
const [removeInput, setRemoveInput] = useState("");
const [packageBusy, setPackageBusy] = useState<"install" | "remove" | "update" | null>(null);
const [packageError, setPackageError] = useState<string | null>(null);
```

Add a handler (near other handlers in the component body):
```tsx
const runPackages = async (action: "install" | "remove" | "update", input?: string) => {
  if (!vm) return;
  const packages = input ? input.split(/\s+/).filter(Boolean) : [];
  if (action !== "update" && packages.length === 0) {
    setPackageError("패키지 이름을 입력하세요.");
    return;
  }
  setPackageBusy(action);
  setPackageError(null);
  try {
    const updated = await runPackageAction(vm.id, action, packages);
    setVm(updated);
    if (action === "install") setInstallInput("");
    if (action === "remove") setRemoveInput("");
  } catch (error) {
    setPackageError((error as Error).message);
  } finally {
    setPackageBusy(null);
  }
};
```

Add a section to the modal's JSX (only when `vm.state === "running"`, near where other running-only actions live — e.g. next to console/log sections):
```tsx
{vm && vm.state === "running" && (
  <section className="panel">
    <h2 className="panel-title">패키지</h2>
    {packageError && <div className="field-error">{packageError}</div>}
    <div className="package-row">
      <input
        type="text"
        placeholder="설치할 패키지 (공백으로 구분)"
        value={installInput}
        onChange={(event) => setInstallInput(event.target.value)}
        disabled={packageBusy !== null}
      />
      <button
        type="button"
        className="btn"
        disabled={packageBusy !== null || !installInput.trim()}
        onClick={() => void runPackages("install", installInput)}
      >
        {packageBusy === "install" ? "설치 중…" : "설치"}
      </button>
    </div>
    <div className="package-row">
      <input
        type="text"
        placeholder="삭제할 패키지 (공백으로 구분)"
        value={removeInput}
        onChange={(event) => setRemoveInput(event.target.value)}
        disabled={packageBusy !== null}
      />
      <button
        type="button"
        className="btn danger"
        disabled={packageBusy !== null || !removeInput.trim()}
        onClick={() => void runPackages("remove", removeInput)}
      >
        {packageBusy === "remove" ? "삭제 중…" : "삭제"}
      </button>
    </div>
    <div className="package-row">
      <button
        type="button"
        className="btn"
        disabled={packageBusy !== null}
        onClick={() => void runPackages("update")}
      >
        {packageBusy === "update" ? "업데이트 중…" : "전체 패키지 업데이트"}
      </button>
    </div>
    {vm.packageUpdate && vm.packageUpdate.status !== "running" && (
      <pre className="detail-log">
        {vm.packageUpdate.status === "succeeded"
          ? vm.packageUpdate.outputTail
          : `${vm.packageUpdate.reason}\n${vm.packageUpdate.outputTail}`}
      </pre>
    )}
  </section>
)}
```
Add `import { runPackageAction } from "../api/client";` to the top of the file (merge into the existing `../api/client` import if one exists).

Add a minimal `.package-row` CSS rule to `firecrab-frontend/src/index.css` if no existing flex-row utility class fits (check for `.store-import-dock` or similar existing flex-row pattern in `Images.tsx`'s CSS and reuse that class name instead of inventing a new one, if its layout — label/input/button in a row — already matches).

- [ ] **Step 4: Manual verification**

Run `npm run build` in `firecrab-frontend/` — must type-check clean (`tsc -b`).
Start the app (see the project's `run` skill or `npm run dev` + `cargo run -p firecrab-api`), open a running VM's detail modal, install a small package (e.g. `curl` on an Alpine VM), confirm the log renders and the VM stays running.

- [ ] **Step 5: Commit**

```bash
git add firecrab-frontend/src/bindings/PackageAction.ts firecrab-frontend/src/bindings/index.ts \
  firecrab-frontend/src/bindings/VmResponse.ts firecrab-frontend/src/api/client.ts \
  firecrab-frontend/src/components/VmDetailModal.tsx firecrab-frontend/src/index.css
git commit -m "feat(frontend): add package install/remove/update to VM detail modal"
```

---

### Task 4: Build session wire types

**Files:**
- Modify: `firecrab-api-types/src/lib.rs`
- Create: `firecrab-frontend/src/bindings/BuildStatus.ts`, `firecrab-frontend/src/bindings/BuildResponse.ts`
- Modify: `firecrab-frontend/src/bindings/index.ts`

**Interfaces:**
- Produces: `firecrab_api_types::{BuildStatus, BuildResponse, FinalizeBuildRequest}`, used by Task 5 (`BuildTracker`) and Task 7–10 (`handlers/builds.rs`).

- [ ] **Step 1: Add the types**

In `firecrab-api-types/src/lib.rs`, near `ImageInstallStatus`/`ImageInstallResponse`:
```rust
/// Lifecycle of one image-build session (`handlers::builds`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    /// Builder VM is being created/started.
    Booting,
    /// VM is running and network-ready; packages can be installed/removed.
    Ready,
    /// A package install/remove/update is in progress on the builder VM.
    Installing,
    /// Stopping the VM and registering the resulting template.
    Finalizing,
    /// Registered as a new template version; builder VM has been deleted.
    Succeeded,
    /// Failed at any stage; see `log`. Builder VM has been deleted.
    Failed,
}

/// Status + log for one build session
/// (`POST /api/images/{alias}/build`, `GET /api/images/builds[/{buildId}]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BuildResponse {
    pub build_id: Uuid,
    /// Source template alias this build started from.
    pub source_alias: String,
    /// Target alias: same as `source_alias` (in-place rebuild) or a new
    /// alias (derived template) — set at `finalize` time from the request,
    /// `None` until finalize is called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_alias: Option<String>,
    /// Builder VM id, so the dashboard can reuse the existing console
    /// WebSocket (`/ws/vms/{id}/console`) to show live boot/package output.
    pub vm_id: Uuid,
    pub status: BuildStatus,
    pub log: String,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    /// Whether at least one package install/remove/update has completed on
    /// this session's VM. `POST .../finalize` refuses a session where this
    /// is still `false`, so an unmodified template can't be re-registered
    /// as if it were new.
    #[serde(default)]
    pub had_package_action: bool,
}

/// Body of `POST /api/images/builds/{buildId}/finalize`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FinalizeBuildRequest {
    /// Omitted → rebuild the source alias in place (new version, same
    /// alias). `Some` → register as a new, distinct alias.
    #[serde(default)]
    pub new_alias: Option<String>,
}
```
(`Uuid` is already imported in this file, matching `CreateVmRequest.micro_network_id`'s usage.)

- [ ] **Step 2: Add TS bindings**

`firecrab-frontend/src/bindings/BuildStatus.ts`:
```ts
// Mirrors firecrab_api_types::BuildStatus (camelCase wire shape).

export type BuildStatus = "booting" | "ready" | "installing" | "finalizing" | "succeeded" | "failed";
```

`firecrab-frontend/src/bindings/BuildResponse.ts`:
```ts
// Mirrors firecrab_api_types::BuildResponse (camelCase wire shape).

import type { BuildStatus } from "./BuildStatus";

export type BuildResponse = {
  buildId: string;
  sourceAlias: string;
  targetAlias?: string;
  vmId: string;
  status: BuildStatus;
  log: string;
  startedAtMs: number;
  endedAtMs?: number;
  hadPackageAction: boolean;
};
```
Add both to `firecrab-frontend/src/bindings/index.ts` (`export * from "./BuildStatus";`, `export * from "./BuildResponse";`).

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p firecrab-api-types`
Run: `cd firecrab-frontend && npx tsc --noEmit`
Expected: both clean (nothing references the new types yet, so no behavior to test here — this task is pure type scaffolding for Tasks 5–11).

- [ ] **Step 4: Commit**

```bash
git add firecrab-api-types/src/lib.rs firecrab-frontend/src/bindings/BuildStatus.ts \
  firecrab-frontend/src/bindings/BuildResponse.ts firecrab-frontend/src/bindings/index.ts
git commit -m "feat: add build session wire types"
```

---

### Task 5: `BuildTracker`

**Files:**
- Create: `firecrab-api/src/builds.rs`
- Modify: `firecrab-api/src/lib.rs` (or wherever modules are declared — add `pub mod builds;`)
- Modify: `firecrab-api/src/state.rs` (add `builds: BuildTracker` field)

**Interfaces:**
- Consumes: `BuildResponse`, `BuildStatus` (Task 4).
- Produces: `BuildTracker::{new, begin, snapshot, get, list, append_log, set_status, finish_ok, finish_err, remove}` — mirrors `ImageInstallTracker`'s shape but keyed by a generated `build_id: Uuid` (not alias, since multiple builds could target the same alias sequentially, and a build needs its own `vm_id`).

- [ ] **Step 1: Write the failing test**

Create `firecrab-api/src/builds.rs` with just the test module first:
```rust
//! In-process tracker for image-build sessions
//! (`POST /api/images/{alias}/build` and friends) — mirrors
//! `image_install::ImageInstallTracker`'s mechanics but keyed by a
//! generated build id, since a builder VM (and its console) is the
//! long-lived resource a session owns, not just an alias name.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use firecrab_api_types::{BuildResponse, BuildStatus};
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct BuildTracker {
    sessions: Arc<Mutex<HashMap<Uuid, BuildResponse>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_then_snapshot_returns_a_booting_session() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("alpine-3.24", Uuid::new_v4());

        let snapshot = tracker.get(build_id).unwrap();
        assert_eq!(snapshot.status, BuildStatus::Booting);
        assert_eq!(snapshot.source_alias, "alpine-3.24");
    }

    #[test]
    fn get_returns_none_for_an_unknown_build_id() {
        let tracker = BuildTracker::default();
        assert!(tracker.get(Uuid::new_v4()).is_none());
    }

    #[test]
    fn set_status_and_append_log_update_the_live_snapshot() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("ubuntu-26.04", Uuid::new_v4());

        tracker.append_log(build_id, "booted, waiting for network");
        tracker.set_status(build_id, BuildStatus::Ready);

        let snapshot = tracker.get(build_id).unwrap();
        assert_eq!(snapshot.status, BuildStatus::Ready);
        assert!(snapshot.log.contains("booted, waiting for network"));
    }

    #[test]
    fn finish_ok_records_target_alias_and_succeeded_status() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("alpine-3.24", Uuid::new_v4());

        tracker.finish_ok(build_id, "my-nginx-base");

        let snapshot = tracker.get(build_id).unwrap();
        assert_eq!(snapshot.status, BuildStatus::Succeeded);
        assert_eq!(snapshot.target_alias.as_deref(), Some("my-nginx-base"));
        assert!(snapshot.ended_at_ms.is_some());
    }

    #[test]
    fn finish_err_records_failed_status_and_reason_in_the_log() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("rocky-9", Uuid::new_v4());

        tracker.finish_err(build_id, "package install failed: exit 1");

        let snapshot = tracker.get(build_id).unwrap();
        assert_eq!(snapshot.status, BuildStatus::Failed);
        assert!(snapshot.log.contains("package install failed"));
    }

    #[test]
    fn list_returns_every_tracked_session() {
        let tracker = BuildTracker::default();
        tracker.begin("alpine-3.24", Uuid::new_v4());
        tracker.begin("ubuntu-26.04", Uuid::new_v4());

        assert_eq!(tracker.list().len(), 2);
    }

    #[test]
    fn had_package_action_starts_false_and_flips_once_marked() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("alpine-3.24", Uuid::new_v4());
        assert!(!tracker.get(build_id).unwrap().had_package_action);

        tracker.mark_package_action_done(build_id);

        assert!(tracker.get(build_id).unwrap().had_package_action);
    }
}
```

Run: `cargo test -p firecrab-api builds:: -- --nocapture`
Expected: FAIL (no methods implemented yet).

- [ ] **Step 2: Implement `BuildTracker`**

Add above the `#[cfg(test)]` module:
```rust
impl BuildTracker {
    /// Registers a new session in `Booting` and returns its id.
    pub fn begin(&self, source_alias: &str, vm_id: Uuid) -> Uuid {
        let build_id = Uuid::new_v4();
        let now = now_ms();
        let session = BuildResponse {
            build_id,
            source_alias: source_alias.to_owned(),
            target_alias: None,
            vm_id,
            status: BuildStatus::Booting,
            log: format!("[{}] builder VM starting", clock(now)),
            started_at_ms: now,
            ended_at_ms: None,
            had_package_action: false,
        };
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(build_id, session);
        build_id
    }

    pub fn get(&self, build_id: Uuid) -> Option<BuildResponse> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&build_id)
            .cloned()
    }

    pub fn list(&self) -> Vec<BuildResponse> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn set_status(&self, build_id: Uuid, status: BuildStatus) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&build_id)
        {
            session.status = status;
        }
    }

    pub fn append_log(&self, build_id: Uuid, line: impl AsRef<str>) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&build_id)
        {
            session.log.push('\n');
            session.log.push_str(&format!("[{}] {}", clock(now_ms()), line.as_ref()));
        }
    }

    pub fn finish_ok(&self, build_id: Uuid, target_alias: &str) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&build_id)
        {
            session.status = BuildStatus::Succeeded;
            session.target_alias = Some(target_alias.to_owned());
            session.ended_at_ms = Some(now_ms());
        }
    }

    pub fn finish_err(&self, build_id: Uuid, reason: impl AsRef<str>) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(session) = sessions.get_mut(&build_id) {
            session.status = BuildStatus::Failed;
            session.log.push('\n');
            session.log.push_str(&format!("[{}] {}", clock(now_ms()), reason.as_ref()));
            session.ended_at_ms = Some(now_ms());
        }
    }

    /// Drops a session from the tracker (cancel path — the caller is
    /// responsible for tearing down the builder VM itself first).
    pub fn remove(&self, build_id: Uuid) {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&build_id);
    }

    /// Marks that a package install/remove/update has completed on this
    /// session — `handlers::builds::finalize_build` (Task 9) refuses to
    /// register a template from a session where this is still `false`.
    pub fn mark_package_action_done(&self, build_id: Uuid) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&build_id)
        {
            session.had_package_action = true;
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn clock(epoch_ms: u64) -> String {
    // Matches image_install.rs's clock() — plain epoch-seconds label, not a
    // full timestamp; good enough for a human skimming the log.
    format!("{}s", epoch_ms / 1000)
}
```

- [ ] **Step 3: Run tests, verify pass**

Run: `cargo test -p firecrab-api builds:: -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Register the module and add to `AppState`**

Find where modules are declared (check `firecrab-api/src/main.rs` or `lib.rs` for `mod image_install;` and add `mod builds;` next to it — same visibility).

In `firecrab-api/src/state.rs`, add to the `AppState` struct:
```rust
    /// Async image-build sessions (`POST /api/images/{alias}/build`).
    pub(crate) builds: crate::builds::BuildTracker,
```
Initialize it wherever `image_installs`/`image_packages` are initialized in `AppState::new`/`with_db_file` (same `Default::default()` pattern).

- [ ] **Step 5: Confirm the crate still builds**

Run: `cargo build -p firecrab-api`
Expected: success (nothing consumes `state.builds` yet — that's Task 7+).

- [ ] **Step 6: Commit**

```bash
cargo fmt -p firecrab-api
git add firecrab-api/src/builds.rs firecrab-api/src/state.rs firecrab-api/src/main.rs
git commit -m "feat: add BuildTracker for image-build sessions"
```

---

### Task 6: Template finalize helper (identity strip + fsck for a new template disk)

**Files:**
- Modify: `firecrab-api/src/rootfs.rs`

**Interfaces:**
- Consumes: `recover_before_specialization` (existing, currently private — make `pub(crate)`), `write_into_image`/`remove_from_image` (existing, already `fn` at crate-private level — reuse directly since this new function lives in the same file), `STRIP_PATHS` (existing).
- Produces: `pub fn finalize_template_disk(rootfs: &Path) -> Result<(), RootfsError>` — used by Task 9 (`builds.rs` finalize handler) after the builder VM is stopped and its disk generation file has been copied out to the new template path.

- [ ] **Step 1: Write the failing test**

Add to `rootfs.rs`'s `#[cfg(test)] mod tests` (reuse whatever ext4 test fixture helper the existing `specialize_guest` tests use — check the file for a `fn make_test_ext4(...)`-style helper first and reuse it rather than duplicating):
```rust
#[test]
fn finalize_template_disk_strips_identity_and_recovers_the_journal() {
    let (_dir, rootfs) = make_test_ext4_with_etc(); // reuse existing test helper
    // Seed a machine-id the way a booted guest would have written one.
    write_into_image(&rootfs, "/etc/machine-id", b"deadbeefdeadbeefdeadbeefdeadbeef\n").unwrap();

    finalize_template_disk(&rootfs).unwrap();

    let output = run_debugfs(&rootfs, "stat /etc/machine-id");
    let stderr = String::from_utf8_lossy(&output.unwrap().stderr);
    assert!(stderr.contains("not found") || stderr.contains("File not found"));
}

#[test]
fn finalize_template_disk_is_idempotent_on_an_already_clean_disk() {
    let (_dir, rootfs) = make_test_ext4_with_etc();
    finalize_template_disk(&rootfs).unwrap();
    // Calling it again on a disk with nothing left to strip must not error.
    finalize_template_disk(&rootfs).unwrap();
}
```
(If no `make_test_ext4_with_etc()` helper exists yet, check how `specialize_guest`'s own tests build their fixture — likely `mkfs.ext4` + `debugfs -w -R "mkdir /etc"`, matching `handlers::vms::test_support::test_state`'s pattern seen earlier. Reuse that exact sequence as a local helper in this test module if `specialize_guest`'s tests don't already expose one.)

Run: `cargo test -p firecrab-api rootfs::tests::finalize_template_disk -- --nocapture`
Expected: FAIL (function doesn't exist).

- [ ] **Step 2: Implement `finalize_template_disk`**

Add near `specialize_guest`:
```rust
/// Prepares a builder VM's finished rootfs disk to become a new template
/// version: recovers the ext4 journal (same as `specialize_guest`), then
/// strips [`STRIP_PATHS`] identity files so every VM created from this new
/// template gets its own fresh hostname/machine-id/SSH host keys instead of
/// inheriting whatever the builder VM generated at boot. Deliberately does
/// NOT set `/etc/hostname` the way `specialize_guest` does — a template has
/// no VM id yet; that happens per-instance at create time.
pub fn finalize_template_disk(rootfs: &Path) -> Result<(), RootfsError> {
    recover_before_specialization(rootfs)?;
    for path in STRIP_PATHS {
        remove_from_image(rootfs, path);
    }
    Ok(())
}
```
Change `fn recover_before_specialization` to `pub(crate) fn recover_before_specialization` only if it isn't already crate-visible (check its current signature — if it's already bare `fn` in the same file, no change needed since `finalize_template_disk` lives in this same module and can call it directly regardless of visibility).

- [ ] **Step 3: Run tests, verify pass**

Run: `cargo test -p firecrab-api rootfs::tests::finalize_template_disk -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
git add firecrab-api/src/rootfs.rs
git commit -m "feat: add finalize_template_disk for web-built templates"
```

---

### Task 7: `POST /api/images/{alias}/build` + `GET /api/images/builds[/{id}]`

**Files:**
- Create: `firecrab-api/src/handlers/builds.rs`
- Modify: `firecrab-api/src/handlers/mod.rs` (add `pub mod builds;`)
- Modify: `firecrab-api/src/server.rs` (routes)

**Interfaces:**
- Consumes: `handlers::vms::create_vm`, `handlers::vms::start_vm_request` (both plain `async fn`s taking axum extractor structs — directly callable, not just HTTP-bound), `TemplateRegistry::resolve_alias`, `state.store.list_micro_networks()` (`MicroNetworkResponse.internet_enabled`), `state.builds` (Task 5), `VmPurpose::Builder` (Task 1).
- Produces: `pub async fn start_build(...) -> Result<(StatusCode, Json<BuildResponse>), AppError>`, `pub async fn list_builds(...) -> Json<Vec<BuildResponse>>`, `pub async fn get_build(...) -> Result<Json<BuildResponse>, AppError>` — consumed by Task 8–10 (same file) and the frontend (Task 11).

- [ ] **Step 1: Write the failing integration test**

Create `firecrab-api/src/handlers/builds.rs`:
```rust
//! Web-triggered image builds: boot a "builder" VM off an installed
//! template, let the dashboard install/remove packages on its console
//! (`handlers::packages::run_package_action`, reused as-is), then snapshot
//! the resulting disk as a new template version (`finalize`, Task 9).
//!
//! A build session's VM is a completely ordinary VM — same create/start/
//! console/delete code path as any dashboard-created instance — tagged
//! `VmPurpose::Builder` so `list_vms` hides it. This avoids reimplementing
//! any part of VM lifecycle, network setup, or console handling for builds.

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{BuildResponse, BuildStatus, CreateVmRequest, EgressPolicy};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::VmPurpose;
use crate::server::RequestId;
use crate::state::AppState;
use crate::templates::TemplateRegistry;

use super::vms::{create_vm, parse_id, start_vm_request};

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse;
    use tempfile::tempdir;

    use super::*;
    use crate::handlers::vms::test_support::test_state;

    #[tokio::test]
    async fn start_build_rejects_an_unknown_source_alias() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = start_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("no-such-alias".to_owned()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }
}
```

`start_build` takes no request body — nothing about *starting* a build needs input beyond the source alias already in the path; the "same alias vs. new alias" choice belongs to `finalize` (`FinalizeBuildRequest`, defined above), since an operator may only decide that after seeing what changed. The frontend (Task 11) still sends `{}` as the POST body, which axum simply ignores for a handler with no `Json`/`ValidatedJson` extractor.

Run: `cargo test -p firecrab-api handlers::builds:: -- --nocapture`
Expected: FAIL (`start_build` not defined, module not registered — expected compile error at this point).

- [ ] **Step 2: Register the module**

In `firecrab-api/src/handlers/mod.rs`, add `pub mod builds;` alongside the existing `pub mod packages;` etc.

Run: `cargo check -p firecrab-api`
Expected: still fails (`start_build` undefined) — confirms the module wiring itself is now correct.

- [ ] **Step 3: Implement `start_build`**

Add to `builds.rs`, above the test module:
```rust
/// `POST /api/images/{alias}/build` — boots a builder VM off `alias`'s
/// currently installed version and registers a new build session. Returns
/// immediately once the VM's `create_vm`/`start_vm_request` calls have been
/// issued; the caller polls `GET /api/images/builds/{buildId}` (and the
/// existing `/ws/vms/{vmId}/console`) for progress.
pub async fn start_build(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(alias): Path<String>,
) -> Result<(StatusCode, Json<BuildResponse>), AppError> {
    let Some(source) = state.templates.resolve_alias(&alias) else {
        return Err(AppError::not_found(request_id.0));
    };

    let micro_network_id = builder_micro_network_id(&state, request_id.0).await?;
    let disk_gb = builder_disk_gb(source.rootfs.length());

    let create_request = CreateVmRequest {
        name: builder_vm_name(&alias),
        template: alias.clone(),
        ram: 1024,
        cpu: 1,
        disk_gb,
        egress_policy: EgressPolicy::Internet,
        micro_network_id,
        storage_root: None,
    };

    let (_status, Json(created)) = create_vm(
        State(state.clone()),
        Extension(request_id),
        ValidatedJson(create_request),
    )
    .await?;

    mark_as_builder(&state, created.id, request_id.0).await?;

    start_vm_request(State(state.clone()), Extension(request_id), Path(created.id.to_string()))
        .await?;

    let build_id = state.builds.begin(&alias, created.id);
    Ok((
        StatusCode::ACCEPTED,
        Json(state.builds.get(build_id).expect("just inserted")),
    ))
}

/// Names the builder VM so it's recognizable if an operator inspects
/// `data/firecrab.db` directly; not shown anywhere in the dashboard since
/// `list_vms` filters `Builder` records out.
fn builder_vm_name(alias: &str) -> String {
    format!("builder-{alias}-{}", &Uuid::new_v4().to_string()[..8])
}

/// Builder VMs need headroom beyond the source rootfs to install new
/// packages into — a fixed 2 GiB margin over the template's own floor,
/// matching `handlers::images::min_disk_gb_for`'s ceiling logic.
fn builder_disk_gb(rootfs_bytes: u64) -> u16 {
    const GIB: u64 = 1024 * 1024 * 1024;
    let floor: u16 = rootfs_bytes.div_ceil(GIB).try_into().unwrap_or(u16::MAX);
    floor.saturating_add(2)
}

/// Picks the first MicroNetwork with internet egress enabled — a build
/// needs to reach the guest's package repositories. Fails clearly rather
/// than silently picking an isolated network a package install would hang
/// against.
async fn builder_micro_network_id(state: &AppState, request_id: Uuid) -> Result<Uuid, AppError> {
    let store = state.store.clone();
    let networks = tokio::task::spawn_blocking(move || store.list_micro_networks())
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|_| AppError::internal(request_id))?;
    networks
        .into_iter()
        .find(|network| network.internet_enabled)
        .map(|network| network.id)
        .ok_or_else(|| {
            AppError::unavailable(
                "no MicroNetwork with internet access exists — create one before building images",
                request_id,
            )
        })
}

/// Flags the just-created VM as a builder so `list_vms` hides it, then
/// persists that change the same way `handlers::vms::persist_update` does.
async fn mark_as_builder(state: &AppState, vm_id: Uuid, request_id: Uuid) -> Result<(), AppError> {
    let record = {
        let mut vms = state.vms.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let vm = vms.get_mut(&vm_id).ok_or_else(|| AppError::internal(request_id))?;
        vm.purpose = VmPurpose::Builder;
        vm.clone()
    };
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.update(&record))
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|_| AppError::internal(request_id))
}

/// `GET /api/images/builds` — every build session this process has tracked
/// (survives VM stop/delete since `BuildTracker` is independent of the VM
/// lifecycle; does not survive an API restart, matching `ImageInstallTracker`).
pub async fn list_builds(State(state): State<AppState>) -> Json<Vec<BuildResponse>> {
    Json(state.builds.list())
}

/// `GET /api/images/builds/{buildId}`.
pub async fn get_build(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(build_id): Path<String>,
) -> Result<Json<BuildResponse>, AppError> {
    let build_id = parse_id(&build_id, request_id.0)?;
    state
        .builds
        .get(build_id)
        .map(Json)
        .ok_or_else(|| AppError::not_found(request_id.0))
}
```

Check `CreateVmRequest`'s exact field list against `firecrab-api-types/src/lib.rs:108` before finalizing this — the plan above assumes `storage_root: Option<String>` as the last field (confirmed present at line ~127 in the earlier read); if the struct has additional required fields not listed here, add them with the same defaults `create_vm`'s own frontend form uses (cpu=1, ram=1024, egress=Internet).

Confirm `AppError::unavailable(reason: &str, request_id: Uuid)` matches the signature already used in `handlers/images.rs`'s `start_image_package` (`AppError::unavailable("FIRECRAB_IMAGE_BASE_URL is not set...", request_id.0)`).

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test -p firecrab-api handlers::builds:: -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Add a success-path test**

```rust
#[tokio::test]
async fn start_build_boots_a_builder_vm_hidden_from_list_vms() {
    let directory = tempdir().unwrap();
    let state = test_state(directory.path()).await;
    // test_state's default fixture template alias is "ubuntu-rootfs-26.04"
    // (see handlers::vms::test_support::test_state) — reuse it as the
    // build source instead of alpine/ubuntu/rocky, which aren't registered
    // in this lightweight fixture.
    crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

    let (status, Json(build)) = start_build(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path("ubuntu-rootfs-26.04".to_owned()),
    )
    .await
    .unwrap();

    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(build.status, BuildStatus::Booting);

    let Json(listed) = crate::handlers::vms::list_vms(State(state)).await;
    assert!(!listed.iter().any(|vm| vm.id == build.vm_id));
}
```
This references `micro_networks::test_support::seed_internet_micro_network` — check whether `handlers/micro_networks.rs` already has a `test_support` module with a network-seeding helper (it has `insert_micro_network`-style fixtures per the earlier grep hit at `micro_networks.rs:709` inserting a seed VM — inspect that test module directly). If no such helper exists, write the smallest possible one there:
```rust
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn seed_internet_micro_network(state: &AppState) -> Uuid {
        let id = Uuid::new_v4();
        state
            .store
            .insert_micro_network(id, "test-net", "172.31.0.0/24", true)
            .expect("seed micro network");
        id
    }
}
```
(Match `insert_micro_network`'s real signature from `persistence.rs:481` — confirm parameter order/types before writing this call.)

- [ ] **Step 6: Run full test suite for the module**

Run: `cargo test -p firecrab-api handlers::builds:: -- --nocapture`
Expected: PASS. This test also exercises real `start_vm_request`, which spawns Firecracker — since `test_state`'s binary path is `/nonexistent-firecracker` by default, the VM will land in `Error` state rather than `Running`, which is fine: this test only asserts the record was created, tagged builder, and hidden from `list_vms`, not that it fully booted.

- [ ] **Step 7: Wire routes in `server.rs`**

Add near the existing `/api/images/{alias}/install` routes:
```rust
        .route(
            "/api/images/{alias}/build",
            post(handlers::builds::start_build),
        )
        .route(
            "/api/images/builds",
            get(handlers::builds::list_builds),
        )
        .route(
            "/api/images/builds/{buildId}",
            get(handlers::builds::get_build).delete(handlers::builds::cancel_build),
        )
```
(`cancel_build` is added in Task 10 — leave this route referencing it now so Task 8/9/10 only need to append methods/handlers, not touch `server.rs` again. If the plan is executed strictly task-by-task, add the `.delete(...)` half of this route in Task 10 instead, and register only `get` here.)

- [ ] **Step 8: Commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
git add firecrab-api/src/handlers/builds.rs firecrab-api/src/handlers/mod.rs \
  firecrab-api/src/handlers/micro_networks.rs firecrab-api/src/server.rs
git commit -m "feat: add POST /api/images/{alias}/build and build session listing"
```

---

### Task 8: `POST /api/images/builds/{buildId}/packages`

**Files:**
- Modify: `firecrab-api/src/handlers/builds.rs`
- Modify: `firecrab-api/src/server.rs`

**Interfaces:**
- Consumes: `handlers::packages::run_package_action` (Task 2) — called directly, not re-implemented.
- Produces: `pub async fn build_packages(...) -> Result<Json<BuildResponse>, AppError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn build_packages_requires_the_builder_vm_to_be_running() {
    let directory = tempdir().unwrap();
    let state = test_state(directory.path()).await;
    crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
    let (_status, Json(build)) = start_build(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path("ubuntu-rootfs-26.04".to_owned()),
    )
    .await
    .unwrap();

    // The fixture Firecracker binary doesn't exist, so the builder VM never
    // reaches Running — build_packages must surface that as a normal
    // conflict, not panic.
    let error = build_packages(
        State(state),
        Extension(RequestId(Uuid::new_v4())),
        Path(build.build_id.to_string()),
        Json(firecrab_api_types::PackageAction {
            action: firecrab_api_types::PackageActionKind::Install,
            packages: vec!["curl".to_owned()],
        }),
    )
    .await
    .unwrap_err();

    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn build_packages_rejects_an_unknown_build_id() {
    let directory = tempdir().unwrap();
    let state = test_state(directory.path()).await;

    let error = build_packages(
        State(state),
        Extension(RequestId(Uuid::new_v4())),
        Path(Uuid::new_v4().to_string()),
        Json(firecrab_api_types::PackageAction {
            action: firecrab_api_types::PackageActionKind::Update,
            packages: Vec::new(),
        }),
    )
    .await
    .unwrap_err();

    assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
}
```

Run: `cargo test -p firecrab-api handlers::builds:: -- --nocapture`
Expected: FAIL (`build_packages` undefined).

- [ ] **Step 2: Implement `build_packages`**

```rust
/// `POST /api/images/builds/{buildId}/packages` — runs one install/remove/
/// update action on the build session's VM by delegating straight to
/// `handlers::packages::run_package_action` (same validation, same
/// sentinel-wait mechanics) and mirrors its resulting `packageUpdate`
/// status into the build session's own log.
pub async fn build_packages(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(build_id): Path<String>,
    Json(body): Json<firecrab_api_types::PackageAction>,
) -> Result<Json<BuildResponse>, AppError> {
    let build_id = parse_id(&build_id, request_id.0)?;
    let session = state
        .builds
        .get(build_id)
        .ok_or_else(|| AppError::not_found(request_id.0))?;

    state.builds.set_status(build_id, BuildStatus::Installing);
    state.builds.append_log(
        build_id,
        format!("{:?} {}", body.action, body.packages.join(" ")),
    );

    let vm_response = super::packages::run_package_action(
        State(state.clone()),
        Extension(request_id),
        Path(session.vm_id.to_string()),
        Json(body),
    )
    .await
    .inspect_err(|_| state.builds.set_status(build_id, BuildStatus::Ready))?;

    // run_package_action detaches the actual console wait onto a spawned
    // task and returns immediately with `Running` — poll it here the same
    // way `packages.rs`'s own tests do, so build_packages's caller gets a
    // definite outcome instead of another poll loop layered on top.
    let outcome = wait_for_package_outcome(&state, session.vm_id).await;
    match outcome {
        Some(firecrab_api_types::PackageUpdateStatus::Succeeded { output_tail }) => {
            state.builds.append_log(build_id, output_tail);
            state.builds.set_status(build_id, BuildStatus::Ready);
        }
        Some(firecrab_api_types::PackageUpdateStatus::Failed { reason, output_tail }) => {
            state.builds.append_log(build_id, format!("{reason}\n{output_tail}"));
            state.builds.set_status(build_id, BuildStatus::Ready);
        }
        _ => state.builds.set_status(build_id, BuildStatus::Ready),
    }

    let _ = vm_response;
    Ok(Json(state.builds.get(build_id).expect("session still tracked")))
}

/// Polls `state.vms[vm_id].package_update` until it leaves `Running` or a
/// bounded number of attempts elapse — `run_package_action`'s console wait
/// runs on a detached task, so this is the only way to observe its result
/// from a caller that itself must return a single HTTP response.
async fn wait_for_package_outcome(
    state: &AppState,
    vm_id: Uuid,
) -> Option<firecrab_api_types::PackageUpdateStatus> {
    for _ in 0..600 {
        let status = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&vm_id)
            .and_then(|vm| vm.package_update.clone());
        match status {
            Some(firecrab_api_types::PackageUpdateStatus::Running) | None => {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            other => return other,
        }
    }
    None
}
```

`state.builds.get(build_id).ok_or_else(...)` above already confirms the build exists before touching the VM — matches the "rejects an unknown build id" test. For the "requires running VM" test: `run_package_action` itself returns `AppError::invalid_state` (409/CONFLICT, per `handlers/packages.rs`'s existing `if vm.state != VmState::Running` check) when the builder VM never reached `Running` — this propagates through the `?` in `build_packages` directly, matching the test's expectation without extra code.

- [ ] **Step 3: Run tests, verify pass**

Run: `cargo test -p firecrab-api handlers::builds:: -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Wire the route**

```rust
        .route(
            "/api/images/builds/{buildId}/packages",
            post(handlers::builds::build_packages),
        )
```

- [ ] **Step 5: Commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
git add firecrab-api/src/handlers/builds.rs firecrab-api/src/server.rs
git commit -m "feat: add POST /api/images/builds/{buildId}/packages"
```

---

### Task 9: `POST /api/images/builds/{buildId}/finalize`

**Files:**
- Modify: `firecrab-api/src/handlers/builds.rs`
- Modify: `firecrab-api/src/server.rs`

**Interfaces:**
- Consumes: `handlers::vms::stop_vm`, `handlers::vms::delete_vm` (both directly callable, same pattern as Task 7), `rootfs::finalize_template_disk` (Task 6), `TemplateRegistry::register_spec` (existing), `crate::artifacts::VmArtifactPaths` (existing — for locating the builder VM's current disk generation file before `delete_vm` removes it).
- Produces: `pub async fn finalize_build(...) -> Result<Json<BuildResponse>, AppError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn finalize_build_rejects_a_session_with_no_successful_package_action() {
    let directory = tempdir().unwrap();
    let state = test_state(directory.path()).await;
    crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
    let (_status, Json(build)) = start_build(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path("ubuntu-rootfs-26.04".to_owned()),
    )
    .await
    .unwrap();

    let error = finalize_build(
        State(state),
        Extension(RequestId(Uuid::new_v4())),
        Path(build.build_id.to_string()),
    )
    .await
    .unwrap_err();

    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
}
```

Run: `cargo test -p firecrab-api handlers::builds::tests::finalize_build_rejects -- --nocapture`
Expected: FAIL (`finalize_build` undefined).

- [ ] **Step 2: Call `mark_package_action_done` from `build_packages`**

`finalize` must refuse a session where no package action ever ran — `BuildResponse.had_package_action` (Task 4) and `BuildTracker::mark_package_action_done` (Task 5) already exist for this. Wire the call: in `build_packages` (`handlers/builds.rs`, Task 8), right after the `match outcome { ... }` block, add:
```rust
    state.builds.mark_package_action_done(build_id);
```
(unconditionally — an attempted-but-failed package action still counts; only a session that never tried anything is refused).

- [ ] **Step 3: Implement `finalize_build`**

```rust
/// `POST /api/images/builds/{buildId}/finalize` — stops the builder VM,
/// pulls its rootfs disk out from under `delete_vm`'s artifact cleanup,
/// strips guest identity, registers it as a new template version, then
/// deletes the builder VM. `newAlias` in the request body determines
/// whether this is an in-place rebuild (omitted) or a derived template
/// (given) — decided here rather than at `start_build` time, since the
/// operator may only know which they want after seeing what changed.
pub async fn finalize_build(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(build_id): Path<String>,
    ValidatedJson(body): ValidatedJson<FinalizeBuildRequest>,
) -> Result<Json<BuildResponse>, AppError> {
    let parsed_build_id = parse_id(&build_id, request_id.0)?;
    let session = state
        .builds
        .get(parsed_build_id)
        .ok_or_else(|| AppError::not_found(request_id.0))?;

    if !session.had_package_action {
        return Err(AppError::conflict(
            "no_changes",
            "install, remove, or update at least one package before saving this build as an image",
            request_id.0,
        ));
    }

    let target_alias = body.new_alias.unwrap_or_else(|| session.source_alias.clone());
    if target_alias != session.source_alias && TemplateRegistry::known_spec(&target_alias).is_some() {
        return Err(AppError::conflict(
            "alias_reserved",
            "that alias name is reserved for a built-in template",
            request_id.0,
        ));
    }

    state.builds.set_status(parsed_build_id, BuildStatus::Finalizing);

    let vm_record = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&session.vm_id)
        .cloned()
        .ok_or_else(|| AppError::internal(request_id.0))?;

    super::vms::stop_vm(State(state.clone()), Extension(request_id), Path(session.vm_id.to_string()))
        .await?;

    let source_version = state
        .templates
        .resolve_alias(&session.source_alias)
        .ok_or_else(|| AppError::internal(request_id.0))?;

    let Some(disk_generation) = vm_record.disk_generation else {
        state.builds.finish_err(parsed_build_id, "builder VM has no disk generation to finalize");
        return Err(AppError::internal(request_id.0));
    };
    let artifact_paths = crate::artifacts::VmArtifactPaths::for_vm(
        &state.vms_dir_for(&vm_record.storage_root),
        session.vm_id,
    );
    let source_disk = artifact_paths.rootfs(disk_generation);

    let version_tag = format!("{}-{}", target_alias, request_id.0.simple());
    let dest_relative = std::path::PathBuf::from("rootfs").join(format!("{target_alias}-{version_tag}.ext4"));
    let dest_path = state.templates.image_root_path().join(&dest_relative);

    let finalize_result = tokio::task::spawn_blocking({
        let source_disk = source_disk.clone();
        let dest_path = dest_path.clone();
        move || -> Result<(), String> {
            std::fs::copy(&source_disk, &dest_path).map_err(|error| {
                format!("copy {} -> {}: {error}", source_disk.display(), dest_path.display())
            })?;
            crate::rootfs::finalize_template_disk(&dest_path)
                .map_err(|error| format!("finalize {}: {error}", dest_path.display()))
        }
    })
    .await
    .map_err(|_| AppError::internal(request_id.0))?;

    if let Err(reason) = finalize_result {
        let _ = std::fs::remove_file(&dest_path);
        state.builds.finish_err(parsed_build_id, &reason);
        let _ = super::vms::delete_vm(
            State(state.clone()),
            Extension(request_id),
            Path(session.vm_id.to_string()),
        )
        .await;
        return Err(AppError::internal(request_id.0));
    }

    let spec = crate::templates::TemplateSpec {
        alias: target_alias.clone(),
        version: version_tag,
        kernel: source_version.kernel.relative_path().to_path_buf(),
        initrd: source_version.initrd.as_ref().map(|artifact| artifact.relative_path().to_path_buf()),
        rootfs: dest_relative,
        boot_args: source_version.boot_args.clone(),
    };
    let templates = state.templates.clone();
    let register_result = tokio::task::spawn_blocking(move || templates.register_spec(spec))
        .await
        .map_err(|_| AppError::internal(request_id.0))?;

    if let Err(error) = register_result {
        let _ = std::fs::remove_file(&dest_path);
        state.builds.finish_err(parsed_build_id, error.to_string());
        let _ = super::vms::delete_vm(
            State(state.clone()),
            Extension(request_id),
            Path(session.vm_id.to_string()),
        )
        .await;
        return Err(AppError::internal(request_id.0));
    }

    super::vms::delete_vm(State(state.clone()), Extension(request_id), Path(session.vm_id.to_string()))
        .await?;

    state.builds.finish_ok(parsed_build_id, &target_alias);
    Ok(Json(state.builds.get(parsed_build_id).expect("session still tracked")))
}
```

`FinalizeBuildRequest` is already defined in Task 4 — no new type needed here, just the handler consuming it.

Before writing this exact code, verify: `VerifiedArtifact::relative_path()` returns `&Path` (confirmed at `templates.rs` — `pub fn relative_path(&self) -> &Path`), `TemplateRegistry::image_root_path()` returns `&Path` (confirmed), `AppState::vms_dir_for` exists (used already in `handlers/vms.rs`'s `delete_vm` — confirmed at line ~558), `TemplateError: std::fmt::Display` (check `templates.rs`'s `#[derive(Error)]` / `#[error(...)]` attributes — thiserror gives `Display` automatically, confirmed by the `#[error("...")]` messages already read).

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p firecrab-api handlers::builds:: -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Add a full success-path test**

This requires a builder VM to actually reach `Running` with a real disk, which `test_state`'s fake Firecracker binary can't do. Instead, test the finalize *disk logic* in isolation by seeding a `VmRecord` with `disk_generation: Some(...)` and a real ext4 file at the expected path (reuse `test_state`'s own rootfs-fixture creation pattern), skipping the `stop_vm`/`start_vm` real-process parts:
```rust
#[tokio::test]
async fn finalize_build_registers_a_new_template_version_when_disk_and_flags_are_ready() {
    let directory = tempdir().unwrap();
    let state = test_state(directory.path()).await;
    crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

    let (_status, Json(build)) = start_build(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path("ubuntu-rootfs-26.04".to_owned()),
    )
    .await
    .unwrap();

    // Simulate a completed package action + a stopped VM with a real disk,
    // since this fixture's Firecracker binary can't actually boot one.
    state.builds.mark_package_action_done(build.build_id);
    let generation = Uuid::new_v4();
    let artifact_paths = crate::artifacts::VmArtifactPaths::for_vm(
        &state.vms_dir_for("default"),
        build.vm_id,
    );
    artifact_paths.ensure_directories().unwrap();
    let disk_path = artifact_paths.rootfs(generation);
    std::process::Command::new("mkfs.ext4")
        .args(["-q", "-F"])
        .arg(&disk_path)
        .arg("8M")
        .status()
        .unwrap();
    {
        let mut vms = state.vms.lock().unwrap();
        let vm = vms.get_mut(&build.vm_id).unwrap();
        vm.disk_generation = Some(generation);
        vm.state = crate::model::VmState::Stopped;
    }

    let Json(finalized) = finalize_build(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(build.build_id.to_string()),
        ValidatedJson(FinalizeBuildRequest { new_alias: Some("my-nginx-base".to_owned()) }),
    )
    .await
    .unwrap();

    assert_eq!(finalized.status, BuildStatus::Succeeded);
    assert_eq!(finalized.target_alias.as_deref(), Some("my-nginx-base"));
    assert!(state.templates.resolve_alias("my-nginx-base").is_some());
}
```
`stop_vm` on an already-`Stopped` VM (rather than `Running`) needs checking against its actual guard clause (`handlers/vms.rs::stop_vm`) — if it errors on a non-running VM, adjust the test to leave `vm.state` as whatever `stop_vm` tolerates, or call `finalize_build`'s internals with the VM already in a state `stop_vm` accepts as a no-op. Check `stop_vm`'s state guard before finalizing this test; if it strictly requires `Running`, this test's fixture VM should instead be left in `Running` (impossible to truly stop without a real process) — in that case, extract finalize's disk-copy-and-register logic into its own smaller `pub(crate) fn finalize_and_register(...)` unit tested directly without going through `stop_vm` at all, and have `finalize_build` call `stop_vm` then that helper. Prefer this extraction regardless — it keeps `finalize_build`'s own test focused on orchestration and puts real coverage on the disk/register logic without fighting the test fixture's fake process.

- [ ] **Step 6: Run tests, verify pass**

Run: `cargo test -p firecrab-api handlers::builds:: -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Wire the route**

```rust
        .route(
            "/api/images/builds/{buildId}/finalize",
            post(handlers::builds::finalize_build),
        )
```

- [ ] **Step 8: Commit**

```bash
cargo fmt -p firecrab-api -p firecrab-api-types
cargo clippy -p firecrab-api -p firecrab-api-types --all-targets -- -D warnings
git add firecrab-api/src/handlers/builds.rs firecrab-api/src/server.rs firecrab-api-types/src/lib.rs
git commit -m "feat: add POST /api/images/builds/{buildId}/finalize"
```

---

### Task 10: `DELETE /api/images/builds/{buildId}` (cancel)

**Files:**
- Modify: `firecrab-api/src/handlers/builds.rs`
- Modify: `firecrab-api/src/server.rs`

**Interfaces:**
- Consumes: `handlers::vms::delete_vm` (already imported via Task 9), `state.builds.remove` (Task 5).
- Produces: `pub async fn cancel_build(...) -> Result<StatusCode, AppError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn cancel_build_deletes_the_builder_vm_and_drops_the_session() {
    let directory = tempdir().unwrap();
    let state = test_state(directory.path()).await;
    crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
    let (_status, Json(build)) = start_build(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path("ubuntu-rootfs-26.04".to_owned()),
    )
    .await
    .unwrap();

    let status = cancel_build(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(build.build_id.to_string()),
    )
    .await
    .unwrap();

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(state.builds.get(build.build_id).is_none());
}

#[tokio::test]
async fn cancel_build_rejects_an_unknown_build_id() {
    let directory = tempdir().unwrap();
    let state = test_state(directory.path()).await;

    let error = cancel_build(
        State(state),
        Extension(RequestId(Uuid::new_v4())),
        Path(Uuid::new_v4().to_string()),
    )
    .await
    .unwrap_err();

    assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
}
```

Run: `cargo test -p firecrab-api handlers::builds::tests::cancel_build -- --nocapture`
Expected: FAIL (`cancel_build` undefined).

- [ ] **Step 2: Implement `cancel_build`**

```rust
/// `DELETE /api/images/builds/{buildId}` — tears down the builder VM
/// without registering anything. Safe to call at any point in a session's
/// lifecycle (booting, ready, mid-install) since it goes through the same
/// `delete_vm` path a user-initiated VM delete would.
pub async fn cancel_build(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(build_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let parsed_build_id = parse_id(&build_id, request_id.0)?;
    let session = state
        .builds
        .get(parsed_build_id)
        .ok_or_else(|| AppError::not_found(request_id.0))?;

    // A VM still `Starting`/`Running` needs `stop_vm` before `delete_vm`
    // accepts it — mirror the frontend's own stop-then-delete sequence for
    // a running instance (`Images.tsx`'s `removeVmsUsingImage`).
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

    state.builds.remove(parsed_build_id);
    Ok(StatusCode::NO_CONTENT)
}
```
Confirm `VmState::can_delete()` is a public method (used already in `handlers/vms.rs::delete_vm`'s guard — confirmed at line ~544).

- [ ] **Step 3: Run tests, verify pass**

Run: `cargo test -p firecrab-api handlers::builds:: -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Wire the route**

Update the route added in Task 7:
```rust
        .route(
            "/api/images/builds/{buildId}",
            get(handlers::builds::get_build).delete(handlers::builds::cancel_build),
        )
```
(If Task 7 already registered `.delete(handlers::builds::cancel_build)` as a forward reference, this step is just confirming it now compiles — no further edit needed.)

- [ ] **Step 5: Full backend check + commit**

```bash
cargo fmt -p firecrab-api
cargo clippy -p firecrab-api --all-targets -- -D warnings
cargo test -p firecrab-api -p firecrab-api-types
git add firecrab-api/src/handlers/builds.rs firecrab-api/src/server.rs
git commit -m "feat: add DELETE /api/images/builds/{buildId} to cancel a build"
```

---

### Task 11: Frontend build client functions

**Files:**
- Modify: `firecrab-frontend/src/api/client.ts`

**Interfaces:**
- Consumes: Task 4 bindings (`BuildResponse`), Task 7–10 routes.
- Produces: `startBuild`, `listBuilds`, `getBuild`, `buildPackages`, `finalizeBuild`, `cancelBuild` — consumed by Task 12.

- [ ] **Step 1: Add the functions**

In `firecrab-frontend/src/api/client.ts`, near the existing image functions:
```ts
/** Boot a builder VM off `alias` (`POST /api/images/{alias}/build`). */
export function startBuild(alias: string): Promise<BuildResponse> {
  return fetchJson(`/api/images/${encodeURIComponent(alias)}/build`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({}),
  });
}

/** List every tracked build session (`GET /api/images/builds`). */
export function listBuilds(): Promise<BuildResponse[]> {
  return fetchJson("/api/images/builds");
}

/** Poll one build session (`GET /api/images/builds/{buildId}`). */
export function getBuild(buildId: string): Promise<BuildResponse> {
  return fetchJson(`/api/images/builds/${encodeURIComponent(buildId)}`);
}

/** Install/remove/update packages on a build session's VM. */
export function buildPackages(
  buildId: string,
  action: PackageActionKind,
  packages: string[] = [],
): Promise<BuildResponse> {
  return fetchJson(`/api/images/builds/${encodeURIComponent(buildId)}/packages`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ action, packages }),
  });
}

/** Register the build's current disk as a template (`POST .../finalize`). */
export function finalizeBuild(buildId: string, newAlias?: string): Promise<BuildResponse> {
  return fetchJson(`/api/images/builds/${encodeURIComponent(buildId)}/finalize`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ newAlias: newAlias ?? null }),
  });
}

/** Cancel a build and delete its builder VM (`DELETE /api/images/builds/{buildId}`). */
export async function cancelBuild(buildId: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/images/builds/${encodeURIComponent(buildId)}`, { method: "DELETE" });
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
}
```
Add `BuildResponse` to the `import type { ... } from "../bindings"` line.

- [ ] **Step 2: Type-check**

Run: `cd firecrab-frontend && npx tsc --noEmit`
Expected: clean (nothing calls these yet — Task 12 wires them into the UI).

- [ ] **Step 3: Commit**

```bash
git add firecrab-frontend/src/api/client.ts
git commit -m "feat(frontend): add build session API client functions"
```

---

### Task 12: Images.tsx — single table + build modal

**Files:**
- Modify: `firecrab-frontend/src/components/Images.tsx` (large rewrite)
- Modify: `firecrab-frontend/src/index.css` (add build-modal rules, remove now-dead `.packer-*` rules — see Task 13)

**Interfaces:**
- Consumes: Task 11's client functions, existing `listImages`/`startImagePackage`/`getImagePackage`/`startImageInstall`/`getImageInstall`/`deleteImage` (unchanged — the "가져오기" download flow is untouched by this feature).
- Produces: the new `Images` default export component (same external contract — no props, rendered by whatever parent currently renders `<Images />`, e.g. `App.tsx`/`Shell.tsx` navigation — confirm via `grep -rn "<Images" firecrab-frontend/src` before starting, no changes expected there).

- [ ] **Step 1: Confirm the mount point is unaffected**

Run: `grep -rn "Images" firecrab-frontend/src/App.tsx firecrab-frontend/src/navigation.ts firecrab-frontend/src/components/Shell.tsx`
Expected: a single `<Images />` (or route entry) with no props passed — confirms this rewrite only needs to preserve the default export signature `export default function Images()`.

- [ ] **Step 2: Replace the file**

This is a full-file rewrite (704 → roughly 350–400 lines), not an incremental diff — the existing `PackerBuildPanel`, `PACKER_STAGES`, `stageLog`/`stageState`/`stageStatusText` helpers, and the two-panel layout are removed entirely per the design spec. Keep unchanged: `PACKER_TEMPLATES` (rename to `KNOWN_TEMPLATES`, still needed for the logo/label lookup in the table and the "새 이미지 빌드" source picker), `keepNewestJobSnapshot`, `packageBasename`, `formatRootfsSize`, `removeVmsUsingImage`, the delete-with-in-use-VMs flow, and the "가져오기" (package→install) logic — all of that is orthogonal to this task and already correct.

```tsx
import { useCallback, useEffect, useState } from "react";
import type { BuildResponse, ImageInstallResponse, ImageResponse, VmResponse } from "../bindings";
import {
  ApiClientError,
  buildPackages,
  cancelBuild,
  deleteImage,
  deleteVm,
  finalizeBuild,
  getBuild,
  getImageInstall,
  getImagePackage,
  listImages,
  listVms,
  startBuild,
  startImageInstall,
  startImagePackage,
  stopVm,
} from "../api/client";
import { logDownloadFilename } from "../lib/textExport";
import LogExportActions from "./LogExportActions";

const KNOWN_TEMPLATES = [
  { alias: "alpine-3.24", label: "Alpine Linux", logoSrc: "https://www.alpinelinux.org/alpinelinux-logo.svg" },
  { alias: "ubuntu-26.04", label: "Ubuntu", logoSrc: "https://assets.ubuntu.com/v1/ff6a9a38-ubuntu-logo-2022.svg" },
  { alias: "rocky-9", label: "Rocky Linux", logoSrc: "https://raw.githubusercontent.com/rocky-linux/branding/main/logo/src/icon-primary.svg" },
] as const;

/** Last path segment of an official package URL for the table cell. */
function packageBasename(url: string): string {
  try {
    const path = new URL(url).pathname;
    const seg = path.split("/").filter(Boolean).pop();
    return seg || url;
  } catch {
    return url;
  }
}

/** Human size for the real rootfs artifact (not the ceiled min-disk floor). */
function formatRootfsSize(bytes: number | undefined | null): string {
  const n = typeof bytes === "number" ? bytes : Number(bytes);
  if (!Number.isFinite(n) || n <= 0) return "—";
  const gib = n / 1024 ** 3;
  if (gib >= 1) {
    const rounded = gib >= 10 || Number.isInteger(gib) ? gib.toFixed(0) : gib.toFixed(2);
    return `${rounded} GiB`;
  }
  const mib = n / 1024 ** 2;
  const rounded = mib >= 10 || Number.isInteger(mib) ? mib.toFixed(0) : mib.toFixed(1);
  return `${rounded} MiB`;
}

function keepNewestJobSnapshot(
  current: ImageInstallResponse | null | undefined,
  incoming: ImageInstallResponse,
): ImageInstallResponse {
  if (!current) return incoming;
  if (incoming.status === "idle" && current.status !== "idle") return current;
  const currentStarted = current.startedAtMs;
  const incomingStarted = incoming.startedAtMs;
  if (currentStarted !== undefined && incomingStarted !== undefined && incomingStarted < currentStarted) {
    return current;
  }
  const currentIsTerminal = current.status === "succeeded" || current.status === "failed";
  if (currentStarted !== undefined && incomingStarted === currentStarted && currentIsTerminal && incoming.status === "running") {
    return current;
  }
  return incoming;
}

/**
 * Build modal: boot a builder VM off `sourceAlias`, install/remove packages
 * on its console, then save the result as a new or updated template.
 */
function BuildModal({
  sourceAlias,
  installedAliases,
  onClose,
  onFinalized,
}: {
  sourceAlias: string;
  installedAliases: string[];
  onClose: () => void;
  onFinalized: () => void;
}) {
  const [build, setBuild] = useState<BuildResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [installInput, setInstallInput] = useState("");
  const [removeInput, setRemoveInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [saveMode, setSaveMode] = useState<"update" | "derive">("update");
  const [newAlias, setNewAlias] = useState("");

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const started = await startBuild(sourceAlias);
        if (!cancelled) setBuild(started);
      } catch (err) {
        if (!cancelled) setError((err as Error).message);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [sourceAlias]);

  useEffect(() => {
    if (!build || build.status === "succeeded" || build.status === "failed") return;
    let cancelled = false;
    const timer = setTimeout(async () => {
      try {
        const snapshot = await getBuild(build.buildId);
        if (!cancelled) setBuild(snapshot);
      } catch {
        /* keep last snapshot */
      }
    }, 1000);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [build]);

  const runPackages = async (action: "install" | "remove", input: string) => {
    if (!build) return;
    const packages = input.split(/\s+/).filter(Boolean);
    if (packages.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const updated = await buildPackages(build.buildId, action, packages);
      setBuild(updated);
      if (action === "install") setInstallInput("");
      if (action === "remove") setRemoveInput("");
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const handleFinalize = async () => {
    if (!build) return;
    if (saveMode === "derive" && !newAlias.trim()) {
      setError("새 이미지 이름을 입력하세요.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await finalizeBuild(build.buildId, saveMode === "derive" ? newAlias.trim() : undefined);
      onFinalized();
      onClose();
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const handleCancel = async () => {
    if (build) {
      try {
        await cancelBuild(build.buildId);
      } catch {
        /* best-effort */
      }
    }
    onClose();
  };

  const ready = build?.status === "ready";
  const aliasTaken = newAlias.trim().length > 0 && installedAliases.includes(newAlias.trim());

  return (
    <div className="modal-overlay" onClick={handleCancel}>
      <div className="modal" onClick={(event) => event.stopPropagation()}>
        <h2 className="panel-title">M2Image-builder — {sourceAlias}</h2>
        {error && <div className="field-error">{error}</div>}
        <div className="state-badge">{build?.status ?? "시작 중…"}</div>
        <pre className="detail-log">{build?.log ?? ""}</pre>
        <div className="package-row">
          <input
            type="text"
            placeholder="설치할 패키지 (공백으로 구분)"
            value={installInput}
            onChange={(event) => setInstallInput(event.target.value)}
            disabled={!ready || busy}
          />
          <button type="button" className="btn" disabled={!ready || busy || !installInput.trim()} onClick={() => void runPackages("install", installInput)}>
            설치
          </button>
        </div>
        <div className="package-row">
          <input
            type="text"
            placeholder="삭제할 패키지 (공백으로 구분)"
            value={removeInput}
            onChange={(event) => setRemoveInput(event.target.value)}
            disabled={!ready || busy}
          />
          <button type="button" className="btn danger" disabled={!ready || busy || !removeInput.trim()} onClick={() => void runPackages("remove", removeInput)}>
            삭제
          </button>
        </div>
        <fieldset className="package-row">
          <label>
            <input type="radio" checked={saveMode === "update"} onChange={() => setSaveMode("update")} />
            같은 이미지 갱신 ({sourceAlias})
          </label>
          <label>
            <input type="radio" checked={saveMode === "derive"} onChange={() => setSaveMode("derive")} />
            새 이미지로 저장
          </label>
          {saveMode === "derive" && (
            <input
              type="text"
              placeholder="새 이미지 이름"
              value={newAlias}
              onChange={(event) => setNewAlias(event.target.value)}
            />
          )}
        </fieldset>
        {aliasTaken && <div className="field-error">이미 사용 중인 이름입니다.</div>}
        <div className="package-row">
          <button type="button" className="btn" onClick={() => void handleCancel()}>
            취소
          </button>
          <button type="button" className="btn primary" disabled={!ready || busy || aliasTaken} onClick={() => void handleFinalize()}>
            {busy ? "저장 중…" : "이미지로 저장"}
          </button>
        </div>
      </div>
    </div>
  );
}

export default function Images() {
  const [images, setImages] = useState<ImageResponse[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyAlias, setBusyAlias] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [packageJobs, setPackageJobs] = useState<Record<string, ImageInstallResponse>>({});
  const [install, setInstall] = useState<ImageInstallResponse | null>(null);
  const [buildSourceAlias, setBuildSourceAlias] = useState<string | null>(null);
  const [newBuildSource, setNewBuildSource] = useState<string>(KNOWN_TEMPLATES[0].alias);

  const refreshList = useCallback(async () => {
    try {
      const next = await listImages();
      setImages(next);
      setListError(null);
    } catch (error) {
      setListError((error as Error).message);
    }
  }, []);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  const handleFetchPackage = async (alias: string) => {
    setBusyAlias(alias);
    setActionError(null);
    try {
      const snap = await startImagePackage(alias);
      setPackageJobs((current) => ({ ...current, [alias]: snap }));
      const poll = async () => {
        const latest = await getImagePackage(alias);
        setPackageJobs((current) => ({ ...current, [alias]: keepNewestJobSnapshot(current[alias], latest) }));
        if (latest.status === "running") setTimeout(() => void poll(), 500);
        else if (latest.status === "succeeded") {
          const installed = await startImageInstall(alias);
          setInstall(installed);
          await refreshList();
        }
      };
      void poll();
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  const removeVmsUsingImage = async (users: VmResponse[]) => {
    for (const vm of users) {
      if (vm.state === "running" || vm.state === "starting") {
        await stopVm(vm.id);
        for (let attempt = 0; attempt < 40; attempt++) {
          await new Promise((resolve) => setTimeout(resolve, 250));
          const latest = (await listVms()).find((entry) => entry.id === vm.id);
          if (!latest) break;
          if (latest.state === "stopped" || latest.state === "error" || latest.state === "created") break;
        }
      }
      const latest = (await listVms()).find((entry) => entry.id === vm.id);
      if (!latest) continue;
      if (latest.state === "stopping" || latest.state === "starting") {
        throw new Error(`VM ${latest.name}이(가) 아직 ${latest.state} 상태입니다. 잠시 후 다시 시도하세요.`);
      }
      await deleteVm(latest.id);
    }
  };

  const handleDelete = async (alias: string) => {
    if (!window.confirm(`'${alias}' 이미지를 삭제할까요?\n레지스트리에서 제거하고 디스크 파일을 지웁니다.`)) return;
    setBusyAlias(alias);
    setActionError(null);
    try {
      try {
        await deleteImage(alias);
      } catch (error) {
        const apiError = error instanceof ApiClientError ? error : null;
        if (apiError?.apiError?.code !== "in_use") throw error;
        const users = (await listVms()).filter((vm) => vm.template === alias);
        if (users.length === 0) throw error;
        const lines = users.map((vm) => `· ${vm.name} [${vm.state}]`).join("\n");
        if (!window.confirm(`'${alias}' 이미지를 쓰는 VM ${users.length}개가 있습니다.\n웹에서 해당 VM을 지운 뒤 이미지를 삭제할까요?\n\n${lines}`)) {
          setActionError(`이미지 삭제 취소됨 — 사용 중인 VM: ${users.map((vm) => vm.name).join(", ")}`);
          return;
        }
        await removeVmsUsingImage(users);
        await deleteImage(alias);
      }
      await refreshList();
      if (install?.alias === alias) setInstall(null);
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  if (images === null && !listError) {
    return <div className="empty">이미지 목록 불러오는 중…</div>;
  }

  const installedAliases = (images ?? []).filter((image) => image.installed).map((image) => image.alias);

  return (
    <div className="stack">
      <section className="panel">
        <h2 className="panel-title">M2Image</h2>
        {listError && <div className="field-error">{listError}</div>}
        {actionError && <div className="field-error">{actionError}</div>}
        <table className="vm-table image-table">
          <thead>
            <tr>
              <th>이미지</th>
              <th>크기</th>
              <th>상태</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {(images ?? []).map((image) => {
              const job = packageJobs[image.alias];
              const fetching = job?.status === "running";
              const statusLabel = image.installed ? "설치됨" : job?.status === "succeeded" ? "패키지 준비됨" : "미설치";
              return (
                <tr key={image.alias}>
                  <td className="mono">{image.alias}</td>
                  <td className="mono">{formatRootfsSize(image.rootfsSizeBytes)}</td>
                  <td>
                    <span className={`state-badge${image.installed ? " running" : ""}`}>{statusLabel}</span>
                  </td>
                  <td className="actions">
                    {image.installed ? (
                      <>
                        <button type="button" className="btn" disabled={busyAlias === image.alias} onClick={() => setBuildSourceAlias(image.alias)}>
                          빌드
                        </button>
                        <button type="button" className="btn danger" disabled={busyAlias === image.alias} onClick={() => void handleDelete(image.alias)}>
                          {busyAlias === image.alias ? "삭제 중…" : "삭제"}
                        </button>
                      </>
                    ) : image.packageUrl ? (
                      <button type="button" className="btn primary" disabled={fetching || busyAlias === image.alias} onClick={() => void handleFetchPackage(image.alias)} title={image.packageUrl}>
                        {fetching ? "가져오는 중…" : `가져오기 (${packageBasename(image.packageUrl)})`}
                      </button>
                    ) : (
                      <span className="poll-note">패키지 URL 없음</span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {install && install.status !== "idle" && (
          <>
            <div className="log-export-bar">
              <span className="log-export-bar-label">이미지 가져오기 로그 — {install.alias}</span>
              <LogExportActions text={install.log} filename={logDownloadFilename("m2image-import", install.alias)} buttonClassName="btn console-bar-btn" disabled={!install.log} />
            </div>
            <pre className="detail-log image-install-log">{install.log}</pre>
          </>
        )}
        <div className="package-row">
          <select value={newBuildSource} onChange={(event) => setNewBuildSource(event.target.value)}>
            {installedAliases.map((alias) => (
              <option key={alias} value={alias}>{alias}</option>
            ))}
          </select>
          <button type="button" className="btn primary" disabled={installedAliases.length === 0} onClick={() => setBuildSourceAlias(newBuildSource)}>
            + 새 이미지 빌드
          </button>
        </div>
      </section>

      {buildSourceAlias && (
        <BuildModal
          sourceAlias={buildSourceAlias}
          installedAliases={installedAliases}
          onClose={() => setBuildSourceAlias(null)}
          onFinalized={() => void refreshList()}
        />
      )}
    </div>
  );
}
```

- [ ] **Step 3: Type-check**

Run: `cd firecrab-frontend && npx tsc --noEmit`
Fix any mismatches against the actual `BuildResponse`/`ImageResponse` field names from Task 4's bindings.

- [ ] **Step 4: Manual browser verification**

Start the backend (`cargo run -p firecrab-api`) and frontend (`npm run dev` in `firecrab-frontend/`). In the browser:
1. Images 화면이 단일 표로 보이는지 확인 (Store/Packer 2패널 사라짐).
2. 설치된 이미지 하나에서 "빌드" 클릭 → 모달이 뜨고 builder VM 부팅 로그가 보이는지 확인.
3. `ready` 상태가 되면 패키지(예: alpine 기반이면 `curl`) 설치 → 로그에 반영되는지 확인.
4. "새 이미지로 저장"으로 새 alias 지정 후 저장 → 모달이 닫히고 표에 새 행이 추가되는지 확인.
5. 새로 만든 이미지로 VM을 생성해 실제로 부팅되는지, 설치한 패키지가 있는지 확인.
6. 취소 흐름: 빌드 중 "취소" → 표/VM 목록에 builder VM이 남지 않는지 확인 (`GET /api/vms`로도 확인).

- [ ] **Step 5: Commit**

```bash
git add firecrab-frontend/src/components/Images.tsx
git commit -m "feat(frontend): rewrite Images screen as single table + build modal"
```

---

### Task 13: Cleanup — dead CSS + docs

**Files:**
- Modify: `firecrab-frontend/src/index.css`
- Modify: `docs/20-guides/m2image-builder.md`
- Modify: `docs/30-tasks/task-m2image-builder.md`

**Interfaces:** None — this task touches no code paths, only removes now-unused styles and documents the new capability.

- [ ] **Step 1: Find and remove dead `.packer-*` CSS**

```bash
grep -n "packer-" firecrab-frontend/src/index.css
```
For each class only referenced by the deleted `PackerBuildPanel`/pipeline JSX (now gone from `Images.tsx` after Task 12), confirm via `grep -rn "<classname>" firecrab-frontend/src` that no remaining `.tsx` file uses it, then delete its CSS block. Keep any class Task 12's `BuildModal`/table still uses (`state-badge`, `vm-table`, `image-table`, `panel`, `btn`, etc. — these predate the Packer panel and are shared).

- [ ] **Step 2: Add `.modal-overlay`/`.modal`/`.package-row` if missing**

Check whether `VmDetailModal.tsx` already defines `.modal-overlay`/`.modal` styles (it's an existing modal component — very likely yes). If so, `BuildModal` (Task 12) reuses those class names as-is and this step is a no-op. If `.package-row` (introduced in Task 3) isn't yet styled, add a minimal flex-row rule:
```css
.package-row {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin-block: 0.5rem;
}
```

- [ ] **Step 3: Update `docs/20-guides/m2image-builder.md`**

Add a section after the existing "결과 확인과 게시 입력" section:
```markdown
## 웹에서 파생 이미지 빌드

설치된 템플릿을 소스로, 대시보드에서 직접 패키지를 설치/삭제하고 새
템플릿(또는 같은 alias의 새 버전)으로 저장할 수 있다. 배포판을 처음부터
부트스트랩하는 것(위 CLI 경로)과는 다른 기능이다 — docker나 root 권한
없이, Firecracker microVM 자체를 빌드 환경으로 쓴다.

1. Images 화면에서 설치된 이미지의 "빌드" 클릭 (또는 "+ 새 이미지 빌드")
2. 부팅된 builder VM 콘솔에서 패키지 설치/삭제
3. "같은 이미지 갱신" 또는 "새 이미지로 저장"(새 alias 입력)
4. 저장하면 새 템플릿 버전이 즉시 VM 생성에 쓸 수 있게 등록됨

커널/initrd는 소스 템플릿 것을 그대로 공유한다 — 웹 빌드는 rootfs(패키지)만
바꾼다. 완전히 새로운 배포판을 처음 추가하는 것은 여전히 이 문서 위쪽의
CLI 경로(`build-m2images.sh`)를 쓴다.
```

- [ ] **Step 4: Update `docs/30-tasks/task-m2image-builder.md`**

Add a bullet under "MVP (제출 범위)":
```markdown
- [x] 웹 대시보드에서 설치된 템플릿을 소스로 파생/리빌드 이미지 빌드
      (builder microVM + 공유 패키지 엔진, docker/신규 특권 데몬 없음) —
      설계: [2026-08-02-m2image-web-builder-design](../superpowers/specs/2026-08-02-m2image-web-builder-design.md)
```

- [ ] **Step 5: Verify docs links**

Run: `python3 scripts/check-doc-links.py` (referenced in the repo's `scripts/` — confirms no broken relative links were introduced).

- [ ] **Step 6: Commit**

```bash
git add firecrab-frontend/src/index.css docs/20-guides/m2image-builder.md docs/30-tasks/task-m2image-builder.md
git commit -m "chore: remove dead Packer pipeline CSS, document web image builds"
```

---

## Final Verification

After all 13 tasks:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd firecrab-frontend && npm run build
```
Then run the manual browser checklist from Task 12 Step 4 once more end-to-end, plus:
- Confirm `GET /api/vms` never lists a `builder-*` VM, even mid-build.
- Confirm the coverage check from memory (`cargo-llvm-cov` after tests pass) if this work is heading toward a PR — patch coverage was previously gated at 78% on the last image-install PR.
