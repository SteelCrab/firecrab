//! Authorized file transport and history over the project's verified retained volume.
use super::{
    files_db::{Operation, Version},
    files_guest::{self as wire, Reply},
    *,
};
use axum::extract::{DefaultBodyLimit, Query};
use tokio::sync::OwnedMutexGuard;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilePath {
    #[serde(default)]
    path: String,
    before: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Comparison {
    path: String,
    from: u64,
    to: u64,
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/files",
            get(read).put(save),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/files/tree",
            get(tree),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/files/history",
            get(history),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/files/versions/{version}",
            get(version),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/files/diff",
            get(diff),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/files/restore",
            post(restore),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/file-operations",
            get(operations),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/file-operations/{operation}/reconcile",
            post(reconcile),
        )
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
}

/// Keep blocking SQLite/filesystem work off the executor, with a bounded number of owned workers.
async fn run<T: Send + 'static>(
    slots: std::sync::Arc<tokio::sync::Semaphore>,
    id: Uuid,
    work: impl std::future::Future<Output = Result<T, AppError>> + Send + 'static,
) -> Result<T, AppError> {
    let permit = slots
        .try_acquire_owned()
        .map_err(|_| Error::Busy.response(id))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        tokio::runtime::Handle::current().block_on(work)
    })
    .await
    .map_err(|_| AppError::internal(id))?
}

/// VM ownership and project SQLite stay held until the bounded guest operation finishes.
struct Files {
    state: AppState,
    caller: Caller,
    writable: bool,
    binding: Binding,
    runtime: guest::Binding,
    digest: String,
    db: db::ProjectDb,
    _vm: OwnedMutexGuard<()>,
}
impl Files {
    async fn open(
        state: AppState,
        project: Uuid,
        caller: Caller,
        writable: bool,
        id: Uuid,
    ) -> Result<Self, AppError> {
        authorize(&state, caller, project, SandboxPermission::Read, id)?;
        if writable {
            authorize(&state, caller, project, SandboxPermission::Write, id)?;
        }
        let local = state.clone();
        let b = blocking(id, move || binding(&local, project)).await?;
        let vm = state.lock_vm(b.vm_id).await;
        authorize(&state, caller, project, SandboxPermission::Read, id)?;
        if writable {
            authorize(&state, caller, project, SandboxPermission::Write, id)?;
        }
        let local = state.clone();
        let saved = b.clone();
        let (db, runtime, digest) = blocking(id, move || {
            if binding(&local, project)? != saved {
                return Err(Error::Conflict("Workspace binding changed"));
            }
            let db = open(&local, &saved)?;
            if response(&local, &saved, &db)?.status != ProjectWorkspaceStatus::Ready {
                return Err(Error::Conflict(
                    "Start the project VM and reconcile its workspace before accessing files",
                ));
            }
            let runtime = guest::binding(&local, saved.vm_id)?;
            let digest = db
                .import_sha()?
                .ok_or(Error::Conflict("Project import identity is missing"))?;
            Ok((db, runtime, digest))
        })
        .await?;
        Ok(Self {
            state,
            caller,
            writable,
            binding: b,
            runtime,
            digest,
            db,
            _vm: vm,
        })
    }

    fn permission(&self) -> Result<(), Error> {
        self.caller
            .project(
                &self.state,
                self.binding.project_id,
                SandboxPermission::Read,
            )
            .map_err(|_| Error::Denied)?;
        if self.writable {
            self.caller
                .project(
                    &self.state,
                    self.binding.project_id,
                    SandboxPermission::Write,
                )
                .map_err(|_| Error::Denied)?;
        }
        Ok(())
    }
    async fn request(&mut self, frame: Vec<u8>) -> Result<Reply, Error> {
        self.permission()?;
        let result = wire::request(
            &self.state,
            &self.binding,
            &self.runtime,
            &self.db.directory,
            &self.digest,
            frame,
        )
        .await?;
        self.permission()?;
        if binding(&self.state, self.binding.project_id)? != self.binding
            || guest::binding(&self.state, self.binding.vm_id).ok() != Some(self.runtime)
        {
            return Err(Error::Conflict(
                "Project runtime changed during file access; reconcile before retrying",
            ));
        }
        Ok(result)
    }

    async fn receipt(&mut self, op: &Operation) -> Result<bool, Error> {
        let expected = wire::operation_frame(&self.db, op)?;
        match self.request(wire::frame(4, "", op.id, None, &[])?).await? {
            Reply::Data(actual) if actual == expected => {
                self.db.finish_file(op.id)?;
                Ok(true)
            }
            Reply::Data(_) => Err(Error::Conflict(
                "Guest receipt does not match the admitted file operation",
            )),
            Reply::Rejected(2) => Ok(false),
            Reply::Rejected(code) => Err(wire::rejected(code)),
        }
    }

    async fn recover_pending(&mut self, path: &str) -> Result<(), Error> {
        if let Some(op) = self.db.pending_file(path)?
            && !self.receipt(&op).await?
        {
            return Err(Error::Conflict(
                "A pending file write needs reconciliation with its original operation identity",
            ));
        }
        Ok(())
    }

    async fn current(&mut self, path: &str) -> Result<Option<Version>, Error> {
        match self
            .request(wire::frame(2, path, Uuid::nil(), None, &[])?)
            .await?
        {
            Reply::Data(bytes) => self.db.observe_file(path, &bytes).map(Some),
            Reply::Rejected(2) => Ok(None),
            Reply::Rejected(code) => Err(wire::rejected(code)),
        }
    }

    async fn apply(&mut self, op: Operation) -> Result<ProjectFile, Error> {
        let frame = wire::operation_frame(&self.db, &op)?;
        match self.request(frame).await? {
            Reply::Data(bytes) if bytes.is_empty() => {
                self.db.finish_file(op.id).map(Version::response)
            }
            Reply::Data(_) => Err(Error::Conflict(
                "Guest file write returned an invalid acknowledgement",
            )),
            Reply::Rejected(code) => {
                if code != 9 {
                    self.db.reject_file(op.id)?;
                }
                Err(wire::rejected(code))
            }
        }
    }
}

fn validate_write(op: &Operation) -> Result<(), Error> {
    wire::path(&op.path, false)?;
    if op.id.is_nil() || op.expected > i64::MAX as u64 || op.content.len() > wire::FILE_LIMIT {
        return Err(Error::Invalid(
            "Use a nonzero operation identity, valid base version and at most 1 MiB",
        ));
    }
    Ok(())
}
fn identical(saved: &Operation, input: &Operation) -> Result<(), Error> {
    if saved.path != input.path
        || saved.expected != input.expected
        || saved.content != input.content
        || saved.restored_from != input.restored_from
        || saved.author != input.author
    {
        return Err(Error::Conflict(
            "File operation identity was already used for a different request",
        ));
    }
    Ok(())
}

async fn write(
    state: AppState,
    project: Uuid,
    caller: Caller,
    input: Operation,
    id: Uuid,
) -> Result<ProjectFile, AppError> {
    authorize(&state, caller, project, SandboxPermission::Write, id)?;
    authorize(&state, caller, project, SandboxPermission::Read, id)?;
    // A confirmed retry remains queryable even after its workload VM stops.
    let local = state.clone();
    let request = input.clone();
    let cached = blocking(id, move || {
        let b = binding(&local, project)?;
        let db = open(&local, &b)?;
        if let Some(op) = db.file_operation(request.id)? {
            identical(&op, &request)?;
            if let Some(version) = op.version {
                return db
                    .file_version(&op.path, version)
                    .map(|v| Some(v.response()));
            }
            if op.state == "failed" {
                return Err(Error::Conflict(
                    "The file operation was rejected; use a new operation identity",
                ));
            }
        }
        Ok(None)
    })
    .await?;
    if let Some(cached) = cached {
        authorize(&state, caller, project, SandboxPermission::Read, id)?;
        authorize(&state, caller, project, SandboxPermission::Write, id)?;
        return Ok(cached);
    }
    let mut files = Files::open(state, project, caller, true, id).await?;
    let result = async {
        if let Some(op) = files.db.file_operation(input.id)? {
            identical(&op, &input)?;
            if let Some(version) = op.version {
                return files
                    .db
                    .file_version(&op.path, version)
                    .map(Version::response);
            }
            if op.state != "pending" {
                return Err(Error::Conflict(
                    "The file operation was rejected; use a new operation identity",
                ));
            }
            return files.apply(op).await;
        }
        files.recover_pending(&input.path).await?;
        let current = files.current(&input.path).await?;
        if current.as_ref().map_or(0, |v| v.metadata.id) != input.expected {
            return Err(Error::Conflict(
                "The file changed; reload before saving with a new operation identity",
            ));
        }
        let op = files.db.stage_file(input)?;
        files.apply(op).await
    }
    .await;
    result.map_err(|e| e.response(id))
}

async fn read(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    Query(query): Query<FilePath>,
) -> Result<Json<ProjectFile>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    wire::path(&query.path, false).map_err(|e| e.response(id.0))?;
    // A dropped browser request does not drop ownership while guest I/O is running.
    run(state.workspaces.file_slots.clone(), id.0, async move {
        let mut files = Files::open(state, project, caller, false, id.0).await?;
        let result = async {
            files.recover_pending(&query.path).await?;
            files
                .current(&query.path)
                .await?
                .ok_or(Error::Missing)
                .map(Version::response)
        }
        .await;
        result.map(Json).map_err(|e| e.response(id.0))
    })
    .await
}

async fn tree(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    Query(query): Query<FilePath>,
) -> Result<Json<Vec<ProjectFileEntry>>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    wire::path(&query.path, true).map_err(|e| e.response(id.0))?;
    run(state.workspaces.file_slots.clone(), id.0, async move {
        let mut files = Files::open(state, project, caller, false, id.0).await?;
        let frame =
            wire::frame(1, &query.path, Uuid::nil(), None, &[]).map_err(|e| e.response(id.0))?;
        match files.request(frame).await.map_err(|e| e.response(id.0))? {
            Reply::Data(bytes) => wire::entries(&bytes)
                .map(Json)
                .map_err(|e| e.response(id.0)),
            Reply::Rejected(code) => Err(wire::rejected(code).response(id.0)),
        }
    })
    .await
}

async fn save(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    Query(query): Query<FilePath>,
    ValidatedJson(input): ValidatedJson<SaveProjectFile>,
) -> Result<Json<ProjectFile>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Write, id.0)?;
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    if input.content_base64.len() > 4 * wire::FILE_LIMIT.div_ceil(3) {
        return Err(Error::Invalid("Files are limited to 1 MiB").response(id.0));
    }
    let content = STANDARD
        .decode(input.content_base64)
        .map_err(|_| Error::Invalid("File contents must use standard base64").response(id.0))?;
    let op = Operation {
        id: input.operation_id,
        path: query.path,
        expected: input.expected_version,
        content,
        author: caller.principal,
        restored_from: None,
        created_at: 0,
        version: None,
        state: "pending".into(),
    };
    validate_write(&op).map_err(|e| e.response(id.0))?;
    run(
        state.workspaces.file_slots.clone(),
        id.0,
        write(state, project, caller, op, id.0),
    )
    .await
    .map(Json)
}

async fn restore(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    Query(query): Query<FilePath>,
    ValidatedJson(input): ValidatedJson<RestoreProjectFile>,
) -> Result<Json<ProjectFile>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Write, id.0)?;
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    wire::path(&query.path, false).map_err(|e| e.response(id.0))?;
    run(state.workspaces.file_slots.clone(), id.0, async move {
        let local = state.clone();
        let path = query.path.clone();
        let bytes = blocking(id.0, move || {
            let b = binding(&local, project)?;
            open(&local, &b)?
                .file_version(&path, input.version)
                .map(|v| v.bytes)
        })
        .await?;
        let op = Operation {
            id: input.operation_id,
            path: query.path,
            expected: input.expected_version,
            content: bytes,
            author: caller.principal,
            restored_from: Some(input.version),
            created_at: 0,
            version: None,
            state: "pending".into(),
        };
        validate_write(&op).map_err(|e| e.response(id.0))?;
        write(state, project, caller, op, id.0).await.map(Json)
    })
    .await
}

async fn history(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    Query(query): Query<FilePath>,
) -> Result<Json<Vec<ProjectFileVersion>>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    wire::path(&query.path, true).map_err(|e| e.response(id.0))?;
    blocking(id.0, move || {
        let b = binding(&state, project)?;
        open(&state, &b)?.file_history(&query.path, query.before)
    })
    .await
    .map(Json)
}
async fn version(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, version)): Path<(Uuid, u64)>,
    Query(query): Query<FilePath>,
) -> Result<Json<ProjectFile>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    wire::path(&query.path, false).map_err(|e| e.response(id.0))?;
    blocking(id.0, move || {
        let b = binding(&state, project)?;
        open(&state, &b)?
            .file_version(&query.path, version)
            .map(Version::response)
    })
    .await
    .map(Json)
}
async fn diff(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    Query(query): Query<Comparison>,
) -> Result<Json<ProjectFileDiff>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    wire::path(&query.path, false).map_err(|e| e.response(id.0))?;
    blocking(id.0, move || {
        let b = binding(&state, project)?;
        let db = open(&state, &b)?;
        Ok(ProjectFileDiff {
            before: db.file_version(&query.path, query.from)?.response(),
            after: db.file_version(&query.path, query.to)?.response(),
        })
    })
    .await
    .map(Json)
}
async fn operations(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    Query(query): Query<FilePath>,
) -> Result<Json<Vec<ProjectFileOperation>>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    wire::path(&query.path, true).map_err(|e| e.response(id.0))?;
    blocking(id.0, move || {
        let b = binding(&state, project)?;
        open(&state, &b)?.file_operations(&query.path)
    })
    .await
    .map(Json)
}
async fn reconcile(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, operation)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectFile>, AppError> {
    let record = authorize(&state, caller, project, SandboxPermission::Write, id.0)?;
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    run(state.workspaces.file_slots.clone(), id.0, async move {
        let local = state.clone();
        let op = blocking(id.0, move || {
            let b = binding(&local, project)?;
            open(&local, &b)?
                .file_operation(operation)?
                .ok_or(Error::Missing)
        })
        .await?;
        if op.author != caller.principal
            && record.owner != caller.principal
            && caller.operator().is_err()
        {
            return Err(denied(id.0));
        }
        if let Some(version) = op.version {
            return blocking(id.0, move || {
                let b = binding(&state, project)?;
                open(&state, &b)?
                    .file_version(&op.path, version)
                    .map(Version::response)
            })
            .await
            .map(Json);
        }
        if op.state != "pending" {
            return Err(Error::Conflict(
                "The file operation was rejected; use a new operation identity",
            )
            .response(id.0));
        }
        let mut files = Files::open(state, project, caller, true, id.0).await?;
        files
            .apply(op)
            .await
            .map(Json)
            .map_err(|e| e.response(id.0))
    })
    .await
}
