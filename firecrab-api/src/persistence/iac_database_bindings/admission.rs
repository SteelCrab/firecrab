use super::*;

fn service_idle(conn: &Connection, id: Uuid) -> Result<(), PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT deployment FROM micro_service_deployments WHERE service_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(raw) = raw {
        let state: firecrab_api_types::MicroServiceDeployment = decode(&raw)?;
        if state.phase.is_busy()
            || state.update.as_ref().is_some_and(|u| !u.completed)
            || !state.instances.is_empty()
            || state.domain_route_owned
            || state.internal_route_owned
        {
            return Err(PersistenceError::IacResourceBusy);
        }
    }
    Ok(())
}
impl Store {
    pub(crate) fn begin_database_binding(
        &self,
        id: Uuid,
        change: BindingChange,
    ) -> Result<Admission, PersistenceError> {
        if id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let request_hash = hash(&change)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(previous) = read(&tx, id)? {
            if previous.request_hash != request_hash {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return Ok(Admission::Ready(Box::new(previous), false));
        }
        let (action, db, input, before) = match change {
            BindingChange::Create { database, input } => {
                if input.request_id.is_nil() || input.service_id.is_nil() {
                    return Ok(Admission::Invalid(
                        "Choose a service and a non-nil binding UUID",
                    ));
                }
                let before = binding(&tx, input.request_id)?;
                if let Some(before) = &before
                    && (before.database_id != database || hash(&before.input)? != hash(&input)?)
                {
                    return Err(PersistenceError::IacOperationIdentity);
                }
                let retired:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM iac_native_operations WHERE kind='database_binding' AND resource_id=?1 AND state='succeeded' AND json_extract(status,'$.action')='delete_database_binding')",[input.request_id.to_string()],|r|r.get(0))?;
                if retired {
                    return Err(PersistenceError::IacOperationIdentity);
                }
                (NativeAction::CreateDatabaseBinding, database, input, before)
            }
            BindingChange::Delete { id } => {
                let Some(before) = binding(&tx, id)? else {
                    return Ok(Admission::Missing);
                };
                (
                    NativeAction::DeleteDatabaseBinding,
                    before.database_id,
                    before.input.clone(),
                    Some(before),
                )
            }
        };
        let target = resource(input.request_id);
        if !iac_owners::public_write_allowed(&tx, &target)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let service = ManagedResource {
            kind: "service".into(),
            id: input.service_id.to_string(),
        };
        if !iac_owners::public_write_allowed(&tx, &service)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        if !super::super::stacks::service_mutation_allowed(&tx, input.service_id, None)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let generated: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM cluster_service_workers WHERE id=?1)",
            [input.service_id.to_string()],
            |r| r.get(0),
        )?;
        if generated {
            return Err(PersistenceError::IacParentManaged);
        }
        service_idle(&tx, input.service_id)?;
        let Some(service_spec) = service_spec(&tx, input.service_id)? else {
            return Ok(Admission::Missing);
        };
        let spec: firecrab_api_types::MicroServiceSpec = decode(&service_spec)?;
        if action == NativeAction::CreateDatabaseBinding
            && (spec.env.contains_key(&input.env_name)
                || spec.secrets.contains_key(&input.env_name))
        {
            return Ok(Admission::Invalid(
                "Remove the environment variable with the same name before binding the database",
            ));
        }
        let raw: Option<String> = tx
            .query_row(
                "SELECT spec FROM pipe_databases WHERE id=?1",
                [db.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(Admission::Missing);
        };
        let database: DatabaseInput = decode(&raw)?;
        let record = DatabaseRecord {
            id: db,
            input: database.clone(),
        };
        let hash_target = crate::pipe_databases::maintenance::target_hash(&record)
            .map_err(|_| PersistenceError::IacOperationIdentity)?;
        super::super::iac_database_targets::idle(&tx, &hash_target, None, false)?;
        if action == NativeAction::DeleteDatabaseBinding && input.env_name == "DATABASE_URL" {
            let referenced:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM database_triggers WHERE json_extract(spec,'$.databaseId')=?1 AND json_extract(spec,'$.serviceId')=?2)",params![db.to_string(),input.service_id.to_string()],|r|r.get(0))?;
            if referenced {
                return Err(PersistenceError::IacManagedDependent);
            }
        }
        let definition = Definition {
            database_id: db,
            database,
            input,
            service_spec_hash: hash(&service_spec)?,
            before,
        };
        let operation = NativeOperation {
            operation_id: id,
            ownership: iac_owners::read(&tx, &target)?,
            action,
            state: NativeOperationState::Running,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
            retain_disks: None,
        };
        iac_native::save(&tx, &operation)?;
        tx.execute("INSERT INTO iac_database_binding_mutations(operation_id,request_hash,definition) VALUES(?1,?2,?3)",params![id.to_string(),request_hash,encode(&definition)?])?;
        tx.execute(
            "INSERT INTO iac_database_targets VALUES(?1,?2)",
            params![hash_target, id.to_string()],
        )?;
        super::super::iac_dependencies::reserve(
            &tx,
            &operation,
            ManagedResource {
                kind: "database_connection".into(),
                id: db.to_string(),
            },
        )?;
        super::super::iac_dependencies::reserve(&tx, &operation, service)?;
        tx.commit()?;
        Ok(Admission::Ready(
            Box::new(BindingIntent {
                operation,
                definition,
                request_hash,
            }),
            true,
        ))
    }
}

/// Transfer preserves the account and grants; it never re-provisions a role.
pub(in crate::persistence) fn validate_owner(
    conn: &Connection,
    target: &ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    let id = Uuid::parse_str(&target.id).map_err(|_| PersistenceError::InvalidIacOwner)?;
    let Some(record) = binding(conn, id)? else {
        return if adopting {
            Err(PersistenceError::InvalidStackResource)
        } else {
            Ok(())
        };
    };
    if record.id != id || record.input.request_id != id {
        return Err(PersistenceError::IacOperationIdentity);
    }
    if !record.ready {
        return Err(PersistenceError::IacResourceBusy);
    }
    let database = ManagedResource {
        kind: "database_connection".into(),
        id: record.database_id.to_string(),
    };
    super::super::iac_resources::database_idle(conn, &database)?;
    let _ = iac_owners::public_write_allowed(
        conn,
        &ManagedResource {
            kind: "service".into(),
            id: record.input.service_id.to_string(),
        },
    )?;
    Ok(())
}
