//! Firecrab's private CA, certificate identity, and TLS configuration.

mod authority;
mod verifier;

pub use authority::{init_ca, issue_host_cert};
pub use rustls;

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use thiserror::Error;
use uuid::Uuid;
use x509_parser::prelude::*;

const MAX_PEM_BYTES: u64 = 256 * 1024;
pub const HOST_URI_PREFIX: &str = "urn:firecrab:host:";

#[derive(Debug, Error)]
#[error("{0}")]
pub struct Error(pub String);

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<rcgen::Error> for Error {
    fn from(error: rcgen::Error) -> Self {
        Self(error.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct TlsPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
}

impl TlsPaths {
    pub fn from_env() -> Result<Option<Self>, Error> {
        Self::from_values(
            std::env::var_os("FIRECRAB_TLS_CERT").map(PathBuf::from),
            std::env::var_os("FIRECRAB_TLS_KEY").map(PathBuf::from),
            std::env::var_os("FIRECRAB_TLS_CA").map(PathBuf::from),
        )
    }

    pub fn from_values(
        cert: Option<PathBuf>,
        key: Option<PathBuf>,
        ca: Option<PathBuf>,
    ) -> Result<Option<Self>, Error> {
        match (cert, key, ca) {
            (None, None, None) => Ok(None),
            (Some(cert), Some(key), Some(ca))
                if [&cert, &key, &ca].iter().all(|p| !p.as_os_str().is_empty()) =>
            {
                Ok(Some(Self { cert, key, ca }))
            }
            _ => Err(Error(
                "set FIRECRAB_TLS_CERT, FIRECRAB_TLS_KEY, and FIRECRAB_TLS_CA together".into(),
            )),
        }
    }

    pub fn server_config(&self) -> Result<ServerConfig, Error> {
        let (certs, key, roots) = self.material()?;
        let verifier = verifier::HostVerifier::new(roots)?;
        let mut config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| Error(e.to_string()))?
                .with_client_cert_verifier(Arc::new(verifier))
                .with_single_cert(certs, key)
                .map_err(|e| Error(e.to_string()))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }

    pub fn client_config(&self) -> Result<ClientConfig, Error> {
        let (certs, key, roots) = self.material()?;
        let mut config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| Error(e.to_string()))?
                .with_root_certificates(roots)
                .with_client_auth_cert(certs, key)
                .map_err(|e| Error(e.to_string()))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }

    /// Verify the normal chain/hostname and the expected Firecrab host UUID.
    pub fn client_config_for_host(&self, id: uuid::Uuid) -> Result<ClientConfig, Error> {
        if id.is_nil() {
            return Err(Error("expected host UUID must not be nil".into()));
        }
        let mut config = self.client_config()?;
        let mut roots = RootCertStore::empty();
        roots
            .add(root_certificate(&self.ca)?)
            .map_err(|e| Error(e.to_string()))?;
        config
            .dangerous()
            .set_certificate_verifier(Arc::new(verifier::ExpectedHostVerifier::new(roots, id)?));
        Ok(config)
    }

    fn material(
        &self,
    ) -> Result<
        (
            Vec<CertificateDer<'static>>,
            PrivateKeyDer<'static>,
            RootCertStore,
        ),
        Error,
    > {
        let certs = certificates(&self.cert)?;
        host_id(&certs[0])?;
        for cert in &certs {
            check_validity(cert)?;
        }
        let ca = root_certificate(&self.ca)?;
        let mut roots = RootCertStore::empty();
        roots.add(ca).map_err(|e| Error(e.to_string()))?;
        let verifier = verifier::HostVerifier::new(roots.clone())?;
        use rustls::server::danger::ClientCertVerifier;
        verifier
            .verify_client_cert(&certs[0], &certs[1..], rustls::pki_types::UnixTime::now())
            .map_err(|e| {
                Error(format!(
                    "local certificate does not chain to the configured CA: {e}"
                ))
            })?;
        let pem = read_private(&self.key)?;
        let key = rustls_pemfile::private_key(&mut &pem[..])?
            .ok_or_else(|| Error("TLS key file contains no private key".into()))?;
        Ok((certs, key, roots))
    }
}

/// Read the opened file's permissions; reject symlinks before following them.
pub fn read_private(path: &Path) -> Result<Vec<u8>, Error> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|e| Error(format!("{}: {e}", path.display())))?;
    check_permissions(&file, path)?;
    let mut bytes = Vec::new();
    file.take(MAX_PEM_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PEM_BYTES {
        return Err(Error(format!("{}: PEM exceeds 256 KiB", path.display())));
    }
    Ok(bytes)
}

fn check_permissions(file: &File, path: &Path) -> Result<(), Error> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error(format!(
            "{}: expected a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(Error(format!(
                "{}: TLS files require mode 0600",
                path.display()
            )));
        }
    }
    Ok(())
}

pub fn certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, Error> {
    let bytes = read_private(path)?;
    let certificates = rustls_pemfile::certs(&mut &bytes[..]).collect::<Result<Vec<_>, _>>()?;
    if certificates.is_empty() {
        return Err(Error(format!("{}: no PEM certificates", path.display())));
    }
    Ok(certificates)
}

pub fn root_certificate(path: &Path) -> Result<CertificateDer<'static>, Error> {
    let mut certs = certificates(path)?;
    if certs.len() != 1 {
        return Err(Error(
            "TLS CA must contain exactly one self-signed root".into(),
        ));
    }
    let cert = certs.remove(0);
    let parsed = parse(&cert)?;
    if !parsed.is_ca()
        || parsed.subject() != parsed.issuer()
        || parsed.verify_signature(None).is_err()
    {
        return Err(Error("TLS CA must be a self-signed CA certificate".into()));
    }
    check_validity(&cert)?;
    Ok(cert)
}

fn parse<'a>(der: &'a [u8]) -> Result<X509Certificate<'a>, Error> {
    let (remaining, certificate) =
        X509Certificate::from_der(der).map_err(|_| Error("invalid X.509 certificate".into()))?;
    if !remaining.is_empty() {
        return Err(Error("trailing X.509 data".into()));
    }
    Ok(certificate)
}

fn check_validity(der: &[u8]) -> Result<(), Error> {
    if !parse(der)?.validity().is_valid() {
        return Err(Error("certificate is expired or not yet valid".into()));
    }
    Ok(())
}

/// This parses identity only; callers must verify trust before accepting a peer.
pub fn host_id(der: &[u8]) -> Result<Uuid, Error> {
    let certificate = parse(der)?;
    let invalid =
        || Error("certificate requires matching UUID CN and urn:firecrab:host:UUID SAN".into());
    let mut names = certificate.subject().iter_common_name();
    let cn = names
        .next()
        .ok_or_else(invalid)?
        .as_str()
        .map_err(|_| invalid())?;
    let id = Uuid::parse_str(cn).map_err(|_| invalid())?;
    if id.is_nil() || names.next().is_some() || cn != id.to_string() {
        return Err(invalid());
    }
    let san = certificate
        .subject_alternative_name()
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    let identities: Vec<_> = san
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::URI(uri) if uri.starts_with(HOST_URI_PREFIX) => Some(*uri),
            _ => None,
        })
        .collect();
    if identities != [format!("{HOST_URI_PREFIX}{id}").as_str()] {
        return Err(invalid());
    }
    Ok(id)
}

pub fn expires_in(path: &Path) -> Result<i64, Error> {
    let certs = certificates(path)?;
    let mut remaining = i64::MAX;
    for der in &certs {
        check_validity(der)?;
        remaining = remaining.min(
            parse(der)?.validity().not_after.timestamp()
                - ::time::OffsetDateTime::now_utc().unix_timestamp(),
        );
    }
    Ok(remaining)
}

#[cfg(test)]
mod tests;
