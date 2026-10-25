//! Ownership and live references for rootfs files created by Deploy.

use super::*;
use crate::oci::{ImageReference, ImageVersion, template_name_from_reference};
use crate::templates::TemplateSpec;
use firecrab_api_types::MicroServiceDeployment;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS deploy_image_cache (
    rootfs TEXT PRIMARY KEY,
    alias TEXT NOT NULL,
    version TEXT NOT NULL,
    registration TEXT NOT NULL
) STRICT";

impl Store {
    /// Record the exact private destination before OCI publishes or registers it.
    pub(crate) fn remember_deploy_image(
        &self,
        spec: &TemplateSpec,
    ) -> Result<(), PersistenceError> {
        if !crate::templates::deploy_cache::owned_rootfs(&spec.rootfs) {
            return Err(corrupt(&spec.alias, "invalid Deploy cache path"));
        }
        let text = serde_json::to_string(spec).map_err(|error| corrupt(&spec.alias, error))?;
        self.lock().execute(
            "INSERT INTO deploy_image_cache (rootfs, alias, version, registration)
            VALUES (?1, ?2, ?3, ?4) ON CONFLICT(rootfs) DO NOTHING",
            params![
                spec.rootfs.to_string_lossy(),
                spec.alias,
                spec.version,
                text
            ],
        )?;
        Ok(())
    }

    pub(crate) fn deploy_images(&self) -> Result<Vec<TemplateSpec>, PersistenceError> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT rootfs, registration FROM deploy_image_cache ORDER BY rowid")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (path, text) = row?;
            let spec: TemplateSpec =
                serde_json::from_str(&text).map_err(|error| corrupt(&path, error))?;
            if spec.rootfs != Path::new(&path)
                || !crate::templates::deploy_cache::owned_rootfs(&spec.rootfs)
            {
                return Err(corrupt(&path, "invalid Deploy cache ownership"));
            }
            Ok(spec)
        })
        .collect()
    }

    pub(crate) fn forget_deploy_image(&self, rootfs: &Path) -> Result<(), PersistenceError> {
        self.lock().execute(
            "DELETE FROM deploy_image_cache WHERE rootfs = ?1",
            [rootfs.to_string_lossy()],
        )?;
        Ok(())
    }

    /// Called while the template registry excludes new readers/registrations.
    /// An already admitted VM/pool creation keeps a template Arc until its row exists.
    pub(crate) fn deploy_image_is_referenced(&self, alias: &str) -> Result<bool, PersistenceError> {
        let conn = self.lock();
        let used: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM vms WHERE template = ?1)
            OR EXISTS(SELECT 1 FROM vm_pools WHERE template = ?1)",
            [alias],
            |row| row.get(0),
        )?;
        if used {
            return Ok(true);
        }
        let mut statement = conn.prepare(
            "SELECT service_id, deployment FROM micro_service_deployments
            UNION ALL SELECT service_id, deployment FROM micro_service_releases",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, text) = row?;
            let deployment: MicroServiceDeployment =
                serde_json::from_str(&text).map_err(|error| corrupt(&id, error))?;
            if deployment_uses(&deployment, alias)? {
                return Ok(true);
            }
        }
        let mut statement = conn.prepare("SELECT id, spec FROM micro_services")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, text) = row?;
            let spec: firecrab_api_types::MicroServiceSpec =
                serde_json::from_str(&text).map_err(|error| corrupt(&id, error))?;
            if image_alias(&spec.image, None)?.as_deref() == Some(alias) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn deployment_uses(
    deployment: &MicroServiceDeployment,
    alias: &str,
) -> Result<bool, PersistenceError> {
    let pending = deployment
        .update
        .as_ref()
        .is_some_and(|update| !update.completed);
    let owns_resources = !deployment.instances.is_empty()
        || deployment.domain_route_owned
        || deployment.internal_route_owned
        || pending;
    if (!matches!(
        deployment.phase,
        firecrab_api_types::ServicePhase::Stopped | firecrab_api_types::ServicePhase::Failed
    ) || owns_resources)
        && image_alias(&deployment.spec.image, deployment.digest.as_deref())?.as_deref()
            == Some(alias)
    {
        return Ok(true);
    }
    if let Some(update) = &deployment.update
        && (!update.completed || !update.previous.instances.is_empty())
    {
        return deployment_uses(&update.previous, alias);
    }
    Ok(false)
}

fn image_alias(image: &str, digest: Option<&str>) -> Result<Option<String>, PersistenceError> {
    let mut reference =
        ImageReference::parse(image).map_err(|error| corrupt("service image", error))?;
    if let Some(digest) = digest {
        reference.version = ImageVersion::Digest(
            crate::oci::Sha256Digest::parse(digest)
                .map_err(|error| corrupt("service image digest", error))?,
        );
    } else if !matches!(reference.version, ImageVersion::Digest(_)) {
        return Ok(None);
    }
    template_name_from_reference(&reference)
        .map(|name| Some(name.alias))
        .map_err(|error| corrupt("service image alias", error))
}

fn corrupt(id: &str, reason: impl ToString) -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: id.into(),
        reason: reason.to_string(),
    }
}
