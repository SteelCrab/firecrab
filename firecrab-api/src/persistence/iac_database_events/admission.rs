use super::*;

impl Store {
    pub(crate) fn begin_database_events(
        &self,
        id: Uuid,
        change: EventChange,
    ) -> Result<Admission, PersistenceError> {
        if id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let request_hash = hash(&change)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(old) = read(&tx, id)? {
            if old.request_hash != request_hash {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return Ok(Admission::Ready(Box::new(old), false));
        }
        let (value, before) = match &change {
            EventChange::Create { database, input } => {
                if input.request_id.is_nil() || input.service_id.is_nil() {
                    return Ok(Admission::Invalid(
                        "Choose a handler and a non-nil event UUID",
                    ));
                }
                let before = trigger(&tx, input.request_id)?;
                if let Some(old) = &before
                    && (old.database_id != *database
                        || hash(&old.input)? != hash(input)?
                        || old.detached)
                {
                    return Err(PersistenceError::IacOperationIdentity);
                }
                let retired:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM iac_native_operations WHERE kind='database_events' AND resource_id=?1 AND state='succeeded' AND json_extract(status,'$.action')='delete_database_events')",[resource(*database,input.request_id).id],|r|r.get(0))?;
                if retired {
                    return Err(PersistenceError::IacOperationIdentity);
                }
                let value = before.clone().unwrap_or_else(|| Trigger {
                    id: input.request_id,
                    database_id: *database,
                    input: input.clone(),
                    paused: true,
                    installed: false,
                    detached: false,
                });
                (value, before)
            }
            EventChange::Pause { id, .. }
            | EventChange::Detach { id }
            | EventChange::Delete { id } => {
                let Some(before) = trigger(&tx, *id)? else {
                    return Ok(Admission::Missing);
                };
                (before.clone(), Some(before))
            }
        };
        match crate::pipe_databases::events::validate_input(&value) {
            Ok(()) => (),
            Err(crate::pipe_databases::write::Error::Invalid(message)) => {
                return Ok(Admission::Invalid(message));
            }
            Err(_) => return Err(PersistenceError::IacOperationIdentity),
        }
        let target = resource(value.database_id, value.id);
        if !iac_owners::public_write_allowed(&tx, &target)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let raw: Option<String> = tx
            .query_row(
                "SELECT spec FROM pipe_databases WHERE id=?1",
                [value.database_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(Admission::Missing);
        };
        let database: DatabaseInput = decode(&raw)?;
        let record = DatabaseRecord {
            id: value.database_id,
            input: database.clone(),
        };
        let target_hash = crate::pipe_databases::maintenance::target_hash(&record)
            .map_err(|_| PersistenceError::IacOperationIdentity)?;
        super::super::iac_database_targets::idle(&tx, &target_hash, None, false)?;
        let broker = || -> Result<Option<RabbitMQInput>, PersistenceError> {
            let raw: Option<String> = tx
                .query_row("SELECT spec FROM database_rabbitmq WHERE id=1", [], |r| {
                    r.get(0)
                })
                .optional()?;
            raw.as_deref().map(decode).transpose()
        };
        let work = match change {
            EventChange::Create { .. } => {
                if !database.write_enabled {
                    return Ok(Admission::Invalid(
                        "Enable database writes before configuring capture",
                    ));
                }
                if !super::super::stacks::service_mutation_allowed(
                    &tx,
                    value.input.service_id,
                    None,
                )? {
                    return Err(PersistenceError::IacOwnershipConflict);
                }
                let spec: Option<String> = tx
                    .query_row(
                        "SELECT spec FROM micro_services WHERE id=?1",
                        [value.input.service_id.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(spec) = spec else {
                    return Ok(Admission::Missing);
                };
                super::super::iac_database_bindings::service_idle(&tx, value.input.service_id)?;
                let generated: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM cluster_service_workers WHERE id=?1)",
                    [value.input.service_id.to_string()],
                    |r| r.get(0),
                )?;
                if generated {
                    return Err(PersistenceError::IacParentManaged);
                }
                let service: firecrab_api_types::MicroServiceSpec = decode(&spec)?;
                if service.env.contains_key("DATABASE_URL")
                    || service.secrets.contains_key("DATABASE_URL")
                {
                    return Ok(Admission::Invalid(
                        "Remove the handler DATABASE_URL override before enabling events",
                    ));
                }
                let Some(broker) = broker()? else {
                    return Ok(Admission::Invalid(
                        "Set up RabbitMQ before enabling triggers",
                    ));
                };
                let raw:Option<String>=tx.query_row("SELECT spec FROM database_bindings WHERE service_id=?1 AND json_extract(spec,'$.input.envName')='DATABASE_URL'",[value.input.service_id.to_string()],|r|r.get(0)).optional()?;
                let binding_before: Option<BindingRecord> =
                    raw.as_deref().map(decode).transpose()?;
                if binding_before
                    .as_ref()
                    .is_some_and(|b| b.database_id != value.database_id)
                {
                    return Ok(Admission::Invalid(
                        "The handler DATABASE_URL belongs to another database",
                    ));
                }
                let binding_id = binding_before.as_ref().map_or(value.id, |b| b.id);
                if binding_before.is_none() {
                    let target = ManagedResource {
                        kind: "database_binding".into(),
                        id: binding_id.to_string(),
                    };
                    let retired:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM iac_native_operations WHERE kind='database_binding' AND resource_id=?1 AND state='succeeded' AND json_extract(status,'$.action')='delete_database_binding')",[binding_id.to_string()],|r|r.get(0))?;
                    if binding(&tx, binding_id)?.is_some()
                        || retired
                        || iac_owners::read(&tx, &target)?.generation != 0
                    {
                        return Err(PersistenceError::IacOperationIdentity);
                    }
                }
                let change_binding = binding_before
                    .as_ref()
                    .is_none_or(|b| !b.ready || !b.event_inbox);
                if change_binding
                    && !iac_owners::public_write_allowed(
                        &tx,
                        &ManagedResource {
                            kind: "database_binding".into(),
                            id: binding_id.to_string(),
                        },
                    )?
                {
                    return Err(PersistenceError::IacOwnershipConflict);
                }
                Work::Create(Box::new(Setup {
                    broker,
                    service_hash: hash(&spec)?,
                    binding_id,
                    binding_before,
                    change_binding,
                }))
            }
            EventChange::Pause { paused, .. } => {
                if !value.installed && !paused {
                    return Ok(Admission::Invalid(
                        "Complete trigger setup before resuming delivery",
                    ));
                }
                Work::Pause(paused)
            }
            EventChange::Detach { .. } => Work::Detach,
            EventChange::Delete { .. } => {
                if !value.detached {
                    return Ok(Admission::Invalid(
                        "Detach source capture before removing a trigger",
                    ));
                }
                let Some(broker) = broker()? else {
                    return Ok(Admission::Invalid(
                        "Reconnect RabbitMQ before removing its owned queues",
                    ));
                };
                Work::Delete(Box::new(broker))
            }
        };
        let operation = NativeOperation {
            operation_id: id,
            ownership: iac_owners::read(&tx, &target)?,
            action: work.action(),
            state: NativeOperationState::Running,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
            retain_disks: None,
        };
        let definition = Definition {
            database,
            trigger: value,
            before,
            work,
        };
        iac_native::save(&tx, &operation)?;
        tx.execute("INSERT INTO iac_database_event_mutations(operation_id,request_hash,definition) VALUES(?1,?2,?3)",params![id.to_string(),request_hash,encode(&definition)?])?;
        tx.execute(
            "INSERT INTO iac_database_targets VALUES(?1,?2)",
            params![target_hash, id.to_string()],
        )?;
        super::super::iac_dependencies::reserve(
            &tx,
            &operation,
            ManagedResource {
                kind: "database_connection".into(),
                id: definition.trigger.database_id.to_string(),
            },
        )?;
        if let Work::Create(setup) = &definition.work {
            super::super::iac_dependencies::reserve(
                &tx,
                &operation,
                ManagedResource {
                    kind: "service".into(),
                    id: definition.trigger.input.service_id.to_string(),
                },
            )?;
            super::super::iac_dependencies::reserve(
                &tx,
                &operation,
                ManagedResource {
                    kind: "database_binding".into(),
                    id: setup.binding_id.to_string(),
                },
            )?;
        }
        if matches!(definition.work, Work::Create(_) | Work::Delete(_)) {
            super::super::iac_dependencies::reserve(
                &tx,
                &operation,
                ManagedResource {
                    kind: "database_rabbitmq".into(),
                    id: "default".into(),
                },
            )?;
        }
        tx.commit()?;
        Ok(Admission::Ready(
            Box::new(EventIntent {
                operation,
                request_hash,
                definition,
            }),
            true,
        ))
    }
}

/// Ownership is qualified by the saved database, never by a display/table name.
pub(in crate::persistence) fn validate_owner(
    conn: &Connection,
    target: &ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    let (database, id) = target
        .id
        .split_once(':')
        .ok_or(PersistenceError::InvalidIacOwner)?;
    let database = Uuid::parse_str(database).map_err(|_| PersistenceError::InvalidIacOwner)?;
    let id = Uuid::parse_str(id).map_err(|_| PersistenceError::InvalidIacOwner)?;
    let Some(value) = trigger(conn, id)? else {
        return if adopting {
            Err(PersistenceError::InvalidStackResource)
        } else {
            Ok(())
        };
    };
    if value.id != id || value.database_id != database || value.input.request_id != id {
        return Err(PersistenceError::InvalidStackResource);
    }
    if !value.installed && !value.detached {
        return Err(PersistenceError::IacResourceBusy);
    }
    super::super::iac_resources::database_idle(
        conn,
        &ManagedResource {
            kind: "database_connection".into(),
            id: database.to_string(),
        },
    )?;
    Ok(())
}
