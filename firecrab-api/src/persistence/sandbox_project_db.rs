//! Route canonical Sandbox records without a second task state machine.
mod migrate;
use super::{
    PersistenceError, Store,
    ai_sandbox::{self, corrupt, decode},
};
use crate::project_workspace::{Binding, db::ProjectDb};
pub(crate) use ai_sandbox::TASK_SCHEMA;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    ops::{Deref, DerefMut},
    sync::MutexGuard,
};
use uuid::Uuid;
pub(super) const SCHEMA:&str="
CREATE TABLE IF NOT EXISTS sandbox_task_catalog(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,project_id TEXT NOT NULL,attempt_id TEXT NOT NULL UNIQUE,ready INTEGER NOT NULL DEFAULT 0 CHECK(ready IN (0,1))) STRICT;
CREATE TABLE IF NOT EXISTS sandbox_task_routes(project_id TEXT PRIMARY KEY,phase TEXT NOT NULL CHECK(phase IN ('prepared','cutover','project')),document TEXT NOT NULL) STRICT;
CREATE TRIGGER IF NOT EXISTS sandbox_task_insert_fence BEFORE INSERT ON ai_sandbox_tasks WHEN EXISTS(SELECT 1 FROM sandbox_task_routes WHERE project_id=NEW.project_id AND phase IN ('prepared','project')) BEGIN SELECT RAISE(ABORT,'Sandbox task storage changed'); END;
CREATE TRIGGER IF NOT EXISTS sandbox_task_update_fence BEFORE UPDATE ON ai_sandbox_tasks WHEN EXISTS(SELECT 1 FROM sandbox_task_routes WHERE project_id=OLD.project_id AND phase IN ('prepared','project')) BEGIN SELECT RAISE(ABORT,'Sandbox task storage changed'); END;
CREATE TRIGGER IF NOT EXISTS sandbox_task_delete_fence BEFORE DELETE ON ai_sandbox_tasks WHEN EXISTS(SELECT 1 FROM sandbox_task_routes WHERE project_id=OLD.project_id AND phase IN ('prepared','project')) BEGIN SELECT RAISE(ABORT,'Sandbox task storage changed'); END;
";
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Route {
    pub binding: Binding,
    pub operation: Uuid,
    pub checkpoint: Uuid,
    pub source_sha: String,
    pub count: usize,
}
fn decode_route(project: &str, raw: String) -> Result<Route, PersistenceError> {
    let route: Route = decode(raw)?;
    if route.binding.project_id.to_string() != project
        || route.operation.is_nil()
        || route.checkpoint.is_nil()
        || route.source_sha.len() != 64
        || !route.source_sha.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(corrupt());
    }
    Ok(route)
}
pub(super) fn unavailable() -> PersistenceError {
    PersistenceError::BlockStorageConflict {
        message: "Sandbox project database is missing, busy, mismatched, or requires migration recovery",
    }
}
// Routing/catalog commits must survive power loss before a project commit can dispatch work.
struct Durable<'a> {
    conn: &'a Connection,
    previous: i64,
}
impl<'a> Durable<'a> {
    fn new(conn: &'a Connection) -> Result<Self, PersistenceError> {
        let previous = conn.pragma_query_value(None, "synchronous", |r| r.get(0))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { conn, previous })
    }
}
impl Drop for Durable<'_> {
    fn drop(&mut self) {
        let _ = self.conn.pragma_update(None, "synchronous", self.previous);
    }
}
impl Store {
    pub(super) fn backfill_sandbox_catalog(&self) -> Result<(), PersistenceError> {
        let c = self.lock();
        let tx = c.unchecked_transaction()?;
        let mut q =
            tx.prepare("SELECT rowid,id,project_id,spec FROM ai_sandbox_tasks ORDER BY rowid")?;
        let mut rows = q.query([])?;
        while let Some(row) = rows.next()? {
            let ordinal: i64 = row.get(0)?;
            let id: String = row.get(1)?;
            let project: String = row.get(2)?;
            let saved: crate::ai_sandbox::SavedTask = decode(row.get(3)?)?;
            if saved.task.id.to_string() != id || saved.task.project_id.to_string() != project {
                return Err(corrupt());
            }
            tx.execute("INSERT OR IGNORE INTO sandbox_task_catalog(ordinal,id,project_id,attempt_id,ready) VALUES(?1,?2,?3,?4,1)",params![ordinal,id,project,saved.task.attempt_id.to_string()])?;
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sandbox_task_catalog WHERE id=?1 AND project_id=?2 AND attempt_id=?3)",params![id,project,saved.task.attempt_id.to_string()],|r|r.get(0))?;
            if !valid {
                return Err(corrupt());
            }
        }
        drop(rows);
        drop(q);
        tx.commit()?;
        Ok(())
    }

    pub(super) fn catalog_task(
        &self,
        t: &crate::ai_sandbox::SavedTask,
    ) -> Result<i64, PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        let tx = c.unchecked_transaction()?;
        tx.execute("INSERT INTO sandbox_task_catalog(id,project_id,attempt_id) SELECT ?1,?2,?3 WHERE NOT EXISTS(SELECT 1 FROM sandbox_task_catalog WHERE id=?1) ON CONFLICT(id) DO NOTHING",params![t.task.id.to_string(),t.task.project_id.to_string(),t.task.attempt_id.to_string()])?;
        let (ordinal, project, attempt): (i64, String, String) = tx.query_row(
            "SELECT ordinal,project_id,attempt_id FROM sandbox_task_catalog WHERE id=?1",
            [t.task.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if project != t.task.project_id.to_string() || attempt != t.task.attempt_id.to_string() {
            return Err(corrupt());
        }
        tx.commit()?;
        Ok(ordinal)
    }
    pub(super) fn task_project(&self, id: Uuid) -> Result<Option<(Uuid, bool)>, PersistenceError> {
        let row: Option<(String, bool)> = self
            .lock()
            .query_row(
                "SELECT project_id,ready FROM sandbox_task_catalog WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(p, r)| Ok((Uuid::parse_str(&p).map_err(|_| corrupt())?, r)))
            .transpose()
    }
    fn routed_db(&self, route: &Route) -> Result<ProjectDb, PersistenceError> {
        let b = self
            .project_workspace(route.binding.project_id)?
            .ok_or_else(unavailable)?;
        if b.database_id != route.binding.database_id || b.volume_id != route.binding.volume_id {
            return Err(unavailable());
        }
        let db = self
            .workspaces
            .open_owned(&b, false)
            .map_err(|error| match error {
                crate::project_workspace::Error::Busy => PersistenceError::ProjectDatabaseBusy,
                _ => unavailable(),
            })?;
        let marker: Option<(String, String)> = db
            .conn
            .query_row(
                "SELECT operation,source_sha FROM sandbox_migration WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if marker != Some((route.operation.to_string(), route.source_sha.clone())) {
            return Err(unavailable());
        }
        Ok(db)
    }
    fn reject_unregistered_target(&self, project: Uuid) -> Result<(), PersistenceError> {
        let Some(b) = self.project_workspace(project)? else {
            return Ok(());
        };
        if b.provisioning
            && std::fs::symlink_metadata(
                self.workspaces
                    .root
                    .join(project.to_string())
                    .join("project.db"),
            )
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(());
        }
        let directory =
            crate::project_workspace::db::project_directory(&self.workspaces.root, &b, false)
                .map_err(|_| unavailable())?;
        let _file =
            crate::project_workspace::db::private_file(&directory.join("project.db"), false)
                .map_err(|_| unavailable())?;
        let conn = Connection::open_with_flags(
            directory.join("project.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(std::time::Duration::from_millis(100))?;
        let (p, d, v): (String, String, String) = conn.query_row(
            "SELECT project_id,database_id,volume_id FROM identity WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let schema: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if p != project.to_string()
            || d != b.database_id.to_string()
            || v != b.volume_id.to_string()
            || schema > crate::project_workspace::db::SCHEMA_VERSION
        {
            return Err(unavailable());
        }
        if schema >= 8
            && conn.query_row("SELECT EXISTS(SELECT 1 FROM sandbox_migration)", [], |r| {
                r.get::<_, bool>(0)
            })?
        {
            return Err(unavailable());
        }
        Ok(())
    }
    pub(super) fn task_connection(
        &self,
        project: Uuid,
        write: bool,
    ) -> Result<TaskConnection<'_>, PersistenceError> {
        let gate = self.sandbox_gate.lock().unwrap_or_else(|p| p.into_inner());
        let has_route: bool = self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM sandbox_task_routes WHERE project_id=?1)",
            [project.to_string()],
            |r| r.get(0),
        )?;
        if !has_route {
            self.reject_unregistered_target(project)?;
        }
        let c = self.lock();
        c.execute_batch("BEGIN DEFERRED")?;
        let row: Result<Option<(String, String)>, rusqlite::Error> = c
            .query_row(
                "SELECT phase,document FROM sandbox_task_routes WHERE project_id=?1",
                [project.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional();
        let row = match row {
            Ok(row) => row,
            Err(e) => {
                let _ = c.execute_batch("ROLLBACK");
                return Err(e.into());
            }
        };
        if row.as_ref().is_none_or(|(phase, _)| phase == "prepared") {
            if write {
                c.execute_batch("ROLLBACK")?;
                if row.is_some() {
                    return Err(unavailable());
                }
            }
            return Ok(TaskConnection {
                db: Database::Legacy(c, !write),
                _gate: gate,
            });
        }
        c.execute_batch("ROLLBACK")?;
        drop(c);
        let (phase, raw) = row.ok_or_else(corrupt)?;
        if phase != "project" {
            return Err(unavailable());
        }
        let route = decode_route(&project.to_string(), raw)?;
        Ok(TaskConnection {
            db: Database::Project(Box::new(self.routed_db(&route)?)),
            _gate: gate,
        })
    }
    /// Snapshot registry and legacy rows together; then query each owned project once.
    pub(super) fn all_task_queries<T>(
        &self,
        mut query: impl FnMut(&Connection) -> Result<T, PersistenceError>,
    ) -> Result<Vec<T>, PersistenceError> {
        let _gate = self.sandbox_gate.lock().unwrap_or_else(|p| p.into_inner());
        let c = self.lock();
        let tx = c.unchecked_transaction()?;
        let mut statement = tx.prepare(
            "SELECT project_id,phase,document FROM sandbox_task_routes ORDER BY project_id",
        )?;
        let mut rows = statement.query([])?;
        let mut routes = Vec::new();
        while let Some(row) = rows.next()? {
            let project: String = row.get(0)?;
            let phase: String = row.get(1)?;
            let route = decode_route(&project, row.get(2)?)?;
            match phase.as_str() {
                "project" => routes.push(route),
                "prepared" => {}
                _ => return Err(unavailable()),
            }
        }
        drop(rows);
        drop(statement);
        let unregistered=tx.prepare("SELECT p.project_id FROM project_workspaces p LEFT JOIN sandbox_task_routes r ON r.project_id=p.project_id WHERE r.project_id IS NULL")?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let mut results = vec![query(&tx)?];
        tx.commit()?;
        drop(c);
        for project in unregistered {
            self.reject_unregistered_target(Uuid::parse_str(&project).map_err(|_| corrupt())?)?;
        }
        for route in routes {
            let db = self.routed_db(&route)?;
            results.push(query(&db.conn)?);
        }
        Ok(results)
    }
    /// Keep the matched volume unchanged while an interrupted handoff is frozen.
    pub(crate) fn sandbox_vm_migration_pending(&self, vm: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row("SELECT EXISTS(SELECT 1 FROM project_workspaces w JOIN sandbox_task_routes r ON r.project_id=w.project_id WHERE w.vm_id=?1 AND r.phase!='project')", [vm.to_string()], |row| row.get(0))?)
    }
    pub(crate) fn sandbox_storage_status(
        &self,
        project: Uuid,
    ) -> Result<serde_json::Value, PersistenceError> {
        let row: Option<(String, String)> = self
            .lock()
            .query_row(
                "SELECT phase,document FROM sandbox_task_routes WHERE project_id=?1",
                [project.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            None => Ok(serde_json::json!({"storage":"controller"})),
            Some((phase, raw)) => {
                let r = decode_route(&project.to_string(), raw)?;
                Ok(
                    serde_json::json!({"storage":phase,"operationId":r.operation,"checkpointId":r.checkpoint,"sourceSha256":r.source_sha,"taskCount":r.count}),
                )
            }
        }
    }
}
enum Database<'a> {
    Legacy(MutexGuard<'a, Connection>, bool),
    Project(Box<ProjectDb>),
}
pub(super) struct TaskConnection<'a> {
    db: Database<'a>,
    _gate: MutexGuard<'a, ()>,
}
impl Deref for TaskConnection<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match &self.db {
            Database::Legacy(c, _) => c,
            Database::Project(db) => &db.conn,
        }
    }
}
impl DerefMut for TaskConnection<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        match &mut self.db {
            Database::Legacy(c, _) => c,
            Database::Project(db) => &mut db.conn,
        }
    }
}
impl Drop for TaskConnection<'_> {
    fn drop(&mut self) {
        if let Database::Legacy(c, true) = &self.db {
            let _ = c.execute_batch("ROLLBACK");
        }
    }
}
