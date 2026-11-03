//! Freeze the generated connector and original parent input before local configuration publication.

use super::*;

/// Private original intent; tokens never enter ownership observations or public snapshots.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PreparedAddress {
    /// Private recovery permit, generated once from the original parent's validated input.
    pub(super) operation_id: Uuid,
    /// Once true, an interrupted attempt cannot be treated as a proven no-effect failure.
    pub(super) effects_started: bool,
    /// Restore the original host protocol when local admission fails before publication begins.
    pub(super) previous_protocol: RegistryProtocol,
    /// Existing parent address, independent of its current connectivity.
    pub(super) subdomain: String,
    /// Exact original target required for a retry.
    target: Target,
    /// Generated once, including when publication fails before a final binding exists.
    pub(super) connector_id: Uuid,
    /// Original saved credential, already retained privately by the address store.
    pub(super) token: String,
    /// Remote protocol cannot silently change while the original publication is uncertain.
    protocol: RegistryProtocol,
}

impl Store {
    /// A retry resolves the original private credential locally before consulting a mutable login API.
    pub(super) fn original_connect(
        &self,
        token: &str,
        target: &Target,
    ) -> Result<Option<(String, RegistryProtocol)>> {
        let Some(original) = self.pending.first() else {
            return Ok(None);
        };
        if self.pending.len() != 1
            || original.token != token
            || original.target != *target
            || original.protocol != self.protocol
        {
            return Err(
                "Recover the original address change with its original credential and target"
                    .into(),
            );
        }
        Ok(Some((original.subdomain.clone(), original.protocol)))
    }
    /// An interrupted retry reuses the original child; another intent cannot replace its reservation.
    pub(super) fn reserve_child(
        &mut self,
        subdomain: &str,
        target: &Target,
        token: &str,
        connector: Uuid,
    ) -> Result<Uuid> {
        if let Some(original) = self
            .pending
            .iter()
            .find(|pending| pending.subdomain == subdomain)
        {
            if original.target != *target
                || original.token != token
                || original.connector_id != connector
                || original.protocol != self.protocol
            {
                return Err(
                    "Recover the original address change before changing its inputs".into(),
                );
            }
            return Ok(original.operation_id);
        }
        if !self.pending.is_empty() {
            return Err(
                "Recover the unfinished Pipe address change before starting another".into(),
            );
        }
        if self.pending.len() >= MAX_ADDRESSES || connector.is_nil() {
            return Err("Address publication reservations are full or invalid".into());
        }
        if self
            .pending
            .iter()
            .any(|pending| pending.connector_id == connector && pending.subdomain != subdomain)
            || self
                .bindings
                .iter()
                .any(|binding| binding.connector_id == connector && binding.subdomain != subdomain)
        {
            return Err("Connector is reserved by another Firecrab address".into());
        }
        let operation_id = Uuid::new_v4();
        self.pending.push(PreparedAddress {
            operation_id,
            effects_started: false,
            previous_protocol: self.protocol,
            subdomain: subdomain.into(),
            target: target.clone(),
            connector_id: connector,
            token: token.into(),
            protocol: self.protocol,
        });
        Ok(operation_id)
    }

    /// An uncertain local publication reserves the shared Pipe target for its original parent.
    pub(super) fn check_pipe_scope(&self, permit: Option<Uuid>) -> Result<()> {
        if self.pending.is_empty() && permit.is_none() {
            return Ok(());
        }
        if self.pending.len() == 1
            && self.pending.first().map(|pending| pending.operation_id) == permit
        {
            return Ok(());
        }
        Err("Recover the original Pipe address operation before changing Pipe configuration or units".into())
    }

    /// Only proven pre-publication failures may remove their original reservation and restore metadata.
    pub(super) fn discard_unstarted(&mut self, operation: Uuid) -> Result<bool> {
        self.check_pipe_scope(Some(operation))?;
        if self.pending[0].effects_started {
            return Ok(false);
        }
        self.protocol = self.pending[0].previous_protocol;
        self.pending.clear();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_marker_survives_restart_and_only_original_unstarted_work_can_be_discarded() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let path = root.join("pipe/firecrab-addresses.json");
        let mut store = Store {
            endpoint: ENDPOINT.into(),
            protocol: RegistryProtocol::AddressesV1,
            bindings: vec![],
            pending: vec![],
        };
        let target = Target::Apps {
            upstreams: vec!["127.0.0.1:8081".parse().unwrap()],
        };
        let operation = store
            .reserve_child(
                "fixture-app",
                &target,
                "fixture-private-token",
                Uuid::new_v4(),
            )
            .unwrap();
        store.pending[0].previous_protocol = RegistryProtocol::RegisterV1;
        assert!(store.discard_unstarted(Uuid::new_v4()).is_err());
        write_at(&path, &store).unwrap();
        mark_address_effects(root, operation).unwrap();
        let mut resumed = read_at(&path).unwrap();
        assert!(resumed.pending[0].effects_started);
        assert!(!resumed.discard_unstarted(operation).unwrap());
        assert_eq!(resumed.pending.len(), 1);
        assert!(store.discard_unstarted(operation).unwrap());
        assert!(store.pending.is_empty());
        assert!(store.protocol == RegistryProtocol::RegisterV1);
    }

    #[test]
    fn original_connector_and_private_input_survive_failed_publication_and_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("addresses.json");
        let mut store = Store {
            endpoint: ENDPOINT.into(),
            protocol: RegistryProtocol::AddressesV1,
            bindings: vec![],
            pending: vec![],
        };
        let id = Uuid::new_v4();
        let target = Target::Apps {
            upstreams: vec!["127.0.0.1:8081".parse().unwrap()],
        };
        store
            .reserve_child("fixture-app", &target, "fixture-private-token", id)
            .unwrap();
        write_at(&path, &store).unwrap();
        let mut restored: Store = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert!(restored.check_pipe_scope(None).is_err());
        assert!(restored.check_pipe_scope(Some(Uuid::new_v4())).is_err());
        assert!(
            restored
                .check_pipe_scope(Some(restored.pending[0].operation_id))
                .is_ok()
        );
        assert_eq!(
            restored
                .original_connect("fixture-private-token", &target)
                .unwrap()
                .unwrap()
                .0,
            "fixture-app"
        );
        assert!(restored.original_connect("changed-token", &target).is_err());
        restored
            .reserve_child("fixture-app", &target, "fixture-private-token", id)
            .unwrap();
        assert_eq!(restored.pending.len(), 1);
        assert_eq!(restored.pending[0].connector_id, id);
        assert!(
            restored
                .reserve_child("fixture-app", &target, "changed-token", id)
                .is_err()
        );
        assert!(
            restored
                .reserve_child(
                    "fixture-app",
                    &target,
                    "fixture-private-token",
                    Uuid::new_v4()
                )
                .is_err()
        );
        assert!(
            restored
                .reserve_child("another-app", &target, "fixture-private-token", id)
                .is_err()
        );
        assert!(
            restored
                .reserve_child(
                    "another-app",
                    &target,
                    "fixture-private-token",
                    Uuid::new_v4()
                )
                .is_err()
        );
        let changed = Target::Apps {
            upstreams: vec!["127.0.0.1:9090".parse().unwrap()],
        };
        assert!(
            restored
                .reserve_child("fixture-app", &changed, "fixture-private-token", id)
                .is_err()
        );
        restored.protocol = RegistryProtocol::RegisterV1;
        assert!(
            restored
                .reserve_child("fixture-app", &target, "fixture-private-token", id)
                .is_err()
        );
    }

    #[test]
    fn background_refresh_does_not_overwrite_an_original_pending_parent_intent() {
        let id = Uuid::new_v4();
        let target = Target::Apps {
            upstreams: vec!["127.0.0.1:8081".parse().unwrap()],
        };
        let binding = Binding {
            subdomain: "fixture-app".into(),
            target: target.clone(),
            connector_id: id,
            owned_connector: true,
            token: "fixture-private-token".into(),
            registered: true,
            connected: true,
            last_success: 42,
            error: None,
        };
        let original = serde_json::to_value(&binding).unwrap();
        let mut store = Store {
            endpoint: "http://127.0.0.1:9".into(),
            protocol: RegistryProtocol::AddressesV1,
            bindings: vec![binding],
            pending: vec![],
        };
        store
            .reserve_child("fixture-app", &target, "fixture-private-token", id)
            .unwrap();
        let snapshot=serde_json::from_value(serde_json::json!({"config":null,"units":[],"nginxAvailable":false,"cloudflaredAvailable":false})).unwrap();
        refresh_bindings(&mut store, &snapshot).unwrap();
        assert_eq!(serde_json::to_value(&store.bindings[0]).unwrap(), original);
        assert_eq!(store.pending.len(), 1);
    }
}
