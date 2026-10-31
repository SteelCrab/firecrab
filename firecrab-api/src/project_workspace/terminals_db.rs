//! Project-owned session metadata, bounded replay and fenced input leases.
use super::{terminals_guest::Reply, *};
use rusqlite::{OptionalExtension, params};
use subtle::ConstantTimeEq;

pub(super) const SCHEMA: &str = "
CREATE TABLE terminal_sessions(id TEXT PRIMARY KEY,record TEXT NOT NULL) STRICT;
CREATE TABLE terminal_output(session TEXT NOT NULL REFERENCES terminal_sessions(id),
 offset INTEGER NOT NULL CHECK(offset>=0),data BLOB NOT NULL,PRIMARY KEY(session,offset)) STRICT;
CREATE TABLE terminal_controls(session TEXT PRIMARY KEY REFERENCES terminal_sessions(id),
 generation INTEGER NOT NULL CHECK(generation>=0),owner TEXT,caller TEXT,client TEXT,token_sha TEXT,
 expires INTEGER NOT NULL CHECK(expires>=0)) STRICT;
";
const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Proof {
    pub boot: Uuid,
    pub uid: u32,
    pub supervisor: u32,
    pub supervisor_ticks: u64,
    pub child: u32,
    pub child_ticks: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Terminal {
    pub id: Uuid,
    pub project: Uuid,
    pub request: CreateProjectTerminal,
    pub creator: Caller,
    pub runtime: guest::Binding,
    pub generation: u64,
    pub proof: Option<Proof>,
    pub status: ProjectTerminalStatus,
    pub start: u64,
    pub end: u64,
    pub guest_end: u64,
    pub truncated: bool,
    pub cleanup_required: bool,
    pub exit: Option<u32>,
}

pub(super) fn now() -> Result<u64, Error> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_secs())
}
fn integer(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::Invalid("Invalid terminal cursor or generation"))
}
fn token_sha(token: Uuid) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

#[derive(Debug)]
struct Lease {
    generation: u64,
    owner: Option<String>,
    caller: Option<Caller>,
    client: Option<Uuid>,
    sha: Option<String>,
    expires: u64,
}
impl Lease {
    fn active(&self, owner: Uuid, time: u64) -> bool {
        self.owner.as_deref() == Some(owner.to_string().as_str())
            && self.expires > time
            && self.expires <= time.saturating_add(60)
            && self.caller.is_some()
            && self.client.is_some()
            && self.sha.is_some()
    }
}

impl db::ProjectDb {
    pub(super) fn terminal(&self, id: Uuid) -> Result<Terminal, Error> {
        self.check_owner()?;
        let json: String = self
            .conn
            .query_row(
                "SELECT record FROM terminal_sessions WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::Missing)?;
        if json.len() > 16384 {
            return Err(Error::Conflict("Invalid terminal metadata"));
        }
        let record: Terminal = serde_json::from_str(&json)?;
        if record.id != id || record.start > record.end || record.end > record.guest_end {
            return Err(Error::Conflict("Invalid terminal identity or replay range"));
        }
        Ok(record)
    }
    pub(super) fn terminals(&self) -> Result<Vec<Terminal>, Error> {
        self.check_owner()?;
        let mut q = self
            .conn
            .prepare("SELECT id FROM terminal_sessions ORDER BY rowid LIMIT 33")?;
        let ids = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if ids.len() > 32 {
            return Err(Error::Conflict("Project terminal limit exceeded"));
        }
        ids.into_iter()
            .map(|id| {
                self.terminal(
                    Uuid::parse_str(&id)
                        .map_err(|_| Error::Conflict("Invalid terminal identity"))?,
                )
            })
            .collect()
    }
    pub(super) fn admit_terminal(&self, record: &Terminal) -> Result<(), Error> {
        self.check_owner()?;
        if self.terminals()?.len() >= 32 {
            return Err(Error::Conflict(
                "Project retains at most 32 terminal histories",
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO terminal_sessions VALUES(?1,?2)",
            params![record.id.to_string(), serde_json::to_string(record)?],
        )?;
        tx.execute(
            "INSERT INTO terminal_controls VALUES(?1,0,NULL,NULL,NULL,NULL,0)",
            [record.id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn save_terminal(&self, record: &Terminal) -> Result<(), Error> {
        self.check_owner()?;
        if self.conn.execute(
            "UPDATE terminal_sessions SET record=?2 WHERE id=?1",
            params![record.id.to_string(), serde_json::to_string(record)?],
        )? != 1
        {
            return Err(Error::Missing);
        }
        Ok(())
    }
    pub(super) fn cache_terminal(&self, record: &mut Terminal, reply: Reply) -> Result<(), Error> {
        self.check_owner()?;
        if let Some(proof) = &record.proof {
            if proof != &reply.proof {
                return Err(Error::Conflict("Terminal process identity changed"));
            }
        } else {
            record.proof = Some(reply.proof);
        }
        if reply.start < record.end
            || reply.end < record.guest_end
            || reply.start > reply.end
            || reply.bytes.len() as u64 > reply.end - reply.start
        {
            return Err(Error::Conflict(
                "Terminal output cursor changed unexpectedly",
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        if reply.start > record.end {
            tx.execute(
                "DELETE FROM terminal_output WHERE session=?1",
                [record.id.to_string()],
            )?;
            record.truncated = true;
            record.start = reply.start;
        }
        if !reply.bytes.is_empty() {
            tx.execute(
                "INSERT INTO terminal_output VALUES(?1,?2,?3)",
                params![record.id.to_string(), integer(reply.start)?, reply.bytes],
            )?;
        }
        record.end = reply.start + reply.bytes.len() as u64;
        record.guest_end = reply.end;
        record.status = reply.status;
        record.exit = reply.exit;
        record.truncated |= reply.truncated;
        record.cleanup_required = reply.cleanup_required;
        let threshold = record.end.saturating_sub(OUTPUT_LIMIT);
        tx.execute(
            "DELETE FROM terminal_output WHERE session=?1 AND offset+length(data)<=?2",
            params![record.id.to_string(), integer(threshold)?],
        )?;
        let first: Option<i64> = tx.query_row(
            "SELECT MIN(offset) FROM terminal_output WHERE session=?1",
            [record.id.to_string()],
            |r| r.get(0),
        )?;
        record.start = first
            .map(|n| u64::try_from(n).map_err(|_| Error::Conflict("Invalid output cursor")))
            .transpose()?
            .unwrap_or(record.end);
        tx.execute(
            "UPDATE terminal_sessions SET record=?2 WHERE id=?1",
            params![record.id.to_string(), serde_json::to_string(record)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn terminal_output(
        &self,
        record: &Terminal,
        cursor: u64,
    ) -> Result<ProjectTerminalOutput, Error> {
        self.check_owner()?;
        if cursor > record.end {
            return Err(Error::Invalid("Terminal cursor is beyond retained output"));
        }
        let start = cursor.max(record.start);
        let mut query = self.conn.prepare("SELECT offset,data FROM terminal_output WHERE session=?1 AND offset+length(data)>?2 ORDER BY offset LIMIT 128")?;
        let mut rows = query.query(params![record.id.to_string(), integer(start)?])?;
        let mut next = start;
        let mut bytes = Vec::new();
        while let Some(row) = rows.next()? {
            let offset = u64::try_from(row.get::<_, i64>(0)?)
                .map_err(|_| Error::Conflict("Invalid replay cursor"))?;
            let data: Vec<u8> = row.get(1)?;
            if offset > next || data.len() > 32768 || next < offset {
                return Err(Error::Conflict("Terminal replay has an unexpected gap"));
            }
            let skip = usize::try_from(next - offset)
                .map_err(|_| Error::Conflict("Invalid replay cursor"))?;
            if skip > data.len() {
                return Err(Error::Conflict("Invalid replay chunk"));
            }
            let count = (32768 - bytes.len()).min(data.len() - skip);
            bytes.extend_from_slice(&data[skip..skip + count]);
            next += count as u64;
            if bytes.len() == 32768 {
                break;
            }
        }
        Ok(ProjectTerminalOutput {
            start,
            next,
            end: record.end,
            truncated: cursor < record.start,
            content_base64: STANDARD.encode(bytes),
        })
    }
    fn terminal_lease(&self, session: Uuid) -> Result<Lease, Error> {
        self.check_owner()?;
        let row = self.conn.query_row("SELECT generation,owner,caller,client,token_sha,expires FROM terminal_controls WHERE session=?1",
            [session.to_string()], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?,r.get::<_, Option<String>>(4)?,r.get::<_, i64>(5)?)))?;
        Ok(Lease {
            generation: u64::try_from(row.0)
                .map_err(|_| Error::Conflict("Invalid control generation"))?,
            owner: row.1,
            caller: row.2.map(|s| serde_json::from_str(&s)).transpose()?,
            client: row
                .3
                .map(|s| {
                    Uuid::parse_str(&s).map_err(|_| Error::Conflict("Invalid controller identity"))
                })
                .transpose()?,
            sha: row.4,
            expires: u64::try_from(row.5).map_err(|_| Error::Conflict("Invalid control expiry"))?,
        })
    }
    pub(super) fn terminal_response(
        &self,
        record: &Terminal,
        owner: Uuid,
    ) -> Result<ProjectTerminal, Error> {
        let lease = self.terminal_lease(record.id)?;
        let controller =
            if record.status == ProjectTerminalStatus::Running && lease.active(owner, now()?) {
                Some(ProjectTerminalController {
                    principal: lease.caller.ok_or(Error::Denied)?.principal,
                    client_id: lease.client.ok_or(Error::Denied)?,
                    generation: lease.generation,
                    expires_at: lease.expires,
                })
            } else {
                None
            };
        Ok(ProjectTerminal {
            id: record.id,
            project_id: record.project,
            vm_id: record.runtime.vm_id,
            runtime_id: record.runtime.runtime_id,
            label: record.request.label.clone(),
            working_directory: record.request.working_directory.clone(),
            creator: record.creator.principal,
            status: record.status,
            boot_id: record.proof.as_ref().map(|p| p.boot),
            exit_code: record.exit,
            output_start: record.start,
            output_end: record.end,
            guest_output_end: record.guest_end,
            truncated: record.truncated,
            cleanup_required: record.cleanup_required,
            control_generation: lease.generation,
            controller,
        })
    }
    pub(super) fn claim_terminal(
        &self,
        session: Uuid,
        owner: Uuid,
        caller: Caller,
        request: ClaimProjectTerminal,
    ) -> Result<ProjectTerminalLease, Error> {
        let record = self.terminal(session)?;
        if record.status != ProjectTerminalStatus::Running {
            return Err(Error::Conflict("This terminal has no live process"));
        }
        let lease = self.terminal_lease(session)?;
        let time = now()?;
        if request.client_id.is_nil()
            || lease.generation != request.expected_generation
            || (lease.active(owner, time) && !request.take_over)
        {
            return Err(Error::Conflict(
                "Terminal control changed; reload before claiming it",
            ));
        }
        let generation = lease.generation.checked_add(1).ok_or(Error::Busy)?;
        let token = Uuid::new_v4();
        let expires = time.checked_add(30).ok_or(Error::Busy)?;
        self.conn.execute("UPDATE terminal_controls SET generation=?2,owner=?3,caller=?4,client=?5,token_sha=?6,expires=?7 WHERE session=?1",
            params![session.to_string(),integer(generation)?,owner.to_string(),serde_json::to_string(&caller)?,request.client_id.to_string(),token_sha(token),integer(expires)?])?;
        Ok(ProjectTerminalLease {
            client_id: request.client_id,
            generation,
            token,
            expires_at: expires,
        })
    }
    pub(super) fn check_terminal_control(
        &self,
        session: Uuid,
        owner: Uuid,
        caller: Caller,
        control: &ProjectTerminalControl,
    ) -> Result<(), Error> {
        let lease = self.terminal_lease(session)?;
        let actual = token_sha(control.token);
        let expected = lease.sha.as_deref().unwrap_or("");
        if !lease.active(owner, now()?)
            || lease.generation != control.generation
            || lease.client != Some(control.client_id)
            || lease.caller.map(|c| c.principal) != Some(caller.principal)
            || !bool::from(actual.as_bytes().ct_eq(expected.as_bytes()))
        {
            return Err(Error::Denied);
        }
        Ok(())
    }
    pub(super) fn renew_terminal(
        &self,
        session: Uuid,
        owner: Uuid,
        caller: Caller,
        control: &ProjectTerminalControl,
        release: bool,
    ) -> Result<(), Error> {
        self.check_terminal_control(session, owner, caller, control)?;
        let expires = if release {
            0
        } else {
            now()?.checked_add(30).ok_or(Error::Busy)?
        };
        self.conn.execute(
            "UPDATE terminal_controls SET expires=?2 WHERE session=?1",
            params![session.to_string(), integer(expires)?],
        )?;
        Ok(())
    }
}
