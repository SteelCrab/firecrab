//! Expire only proven unadmitted input. Unknown or admitted effects keep their recovery material.

use super::*;
use std::collections::BTreeSet;

fn expired(
    conn: &Connection,
    contents: &Contents,
    now: u64,
) -> Result<Vec<Uuid>, PersistenceError> {
    let endpoint = iac_owners::endpoint(conn)?;
    let ids: BTreeSet<_> = contents
        .iac_plan_inputs
        .keys()
        .chain(contents.iac_plan_executions.keys())
        .copied()
        .collect();
    let mut removable = Vec::new();
    for id in ids {
        let input = contents.iac_plan_inputs.get(&id);
        let execution = contents.iac_plan_executions.get(&id);
        let recorded = input
            .map(|value| &value.scope)
            .or_else(|| execution.map(|value| &value.scope))
            .ok_or(PersistenceError::IacOperationIdentity)?;
        // A restored or different database cannot expire another endpoint's material.
        if recorded.ownership.endpoint_id != endpoint {
            continue;
        }
        let Some(frozen) = read(conn, id)? else {
            continue;
        };
        let expected = scope(&frozen)?;
        if recorded != &expected
            || input.is_some_and(|value| value.scope != expected)
            || execution.is_some_and(|value| value.scope != expected)
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let receipt: Option<String> = conn.query_row(
            "SELECT receipt FROM iac_resource_plans WHERE operation_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if let Some(receipt) = receipt {
            let receipt: ResourcePlanReceipt = decode(&receipt)?;
            if receipt.plan != frozen.summary()? {
                return Err(PersistenceError::IacOperationIdentity);
            }
            removable.push(id);
            continue;
        }
        let admitted: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM iac_native_operations WHERE operation_id=?1)",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if !admitted && now >= frozen.expires {
            removable.push(id);
        }
    }
    Ok(removable)
}

impl Store {
    /// Count expired/completed plan scopes, retaining all admitted and unknown effects.
    pub(crate) fn prune_resource_plan_inputs(
        &self,
        vault: &Vault,
        now: u64,
    ) -> Result<usize, PersistenceError> {
        if !vault.is_initialized() {
            return Ok(0);
        }
        vault
            .exclusive(|locked| {
                let removable = {
                    let conn = self.lock();
                    let tx =
                        rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Deferred)
                            .map_err(|_| SecretError::Storage)?;
                    expired(&tx, &locked.contents, now)
                };
                let removable = match removable {
                    Ok(value) => value,
                    Err(error) => return Ok(Err(error)),
                };
                if removable.is_empty() {
                    return Ok(Ok(0));
                }
                for id in &removable {
                    locked.contents.iac_plan_inputs.remove(id);
                    locked.contents.iac_plan_executions.remove(id);
                }
                locked.commit()?;
                Ok(Ok(removable.len()))
            })
            .map_err(|_| PersistenceError::IacVaultUnavailable)?
    }
}
