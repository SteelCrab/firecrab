//! The HTTPS API and WSS console share the same private CA and client identity.

use firecrab_pki::{Error, TlsPaths, rustls::ClientConfig};

pub fn client_config() -> Result<ClientConfig, Error> {
    TlsPaths::from_env()?
        .ok_or_else(|| {
            Error(
                "HTTPS/WSS requires FIRECRAB_TLS_CERT, FIRECRAB_TLS_KEY, and FIRECRAB_TLS_CA"
                    .into(),
            )
        })?
        .client_config()
}

/// Returns the shortest remaining validity across the configured leaf and CA.
#[cfg(any(target_os = "linux", test))]
pub fn expiry(paths: Option<&TlsPaths>) -> Result<Option<i64>, Error> {
    let Some(paths) = paths else {
        return Ok(None);
    };
    paths.client_config()?;
    Ok(Some(
        firecrab_pki::expires_in(&paths.cert)?.min(firecrab_pki::expires_in(&paths.ca)?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_distinguishes_unconfigured_soon_and_healthy_certificates() {
        assert_eq!(expiry(None).unwrap(), None);
        let root = tempfile::tempdir().unwrap();
        let ca = root.path().join("ca");
        firecrab_pki::init_ca(&ca).unwrap();
        for days in [7, 90] {
            let paths =
                firecrab_pki::issue_host_cert(&ca, uuid::Uuid::new_v4(), vec![], days).unwrap();
            let remaining = expiry(Some(&paths)).unwrap().unwrap();
            assert_eq!(remaining > 30 * 86400, days == 90);
        }
    }
}
