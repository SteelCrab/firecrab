use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, PublicKeyData, SanType,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{Error, HOST_URI_PREFIX, TlsPaths, parse, read_private, root_certificate};

fn new_directory(path: &Path) -> Result<(), Error> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(path).map_err(|e| {
        Error(format!(
            "{}: {e}; existing certificates are never overwritten",
            path.display()
        ))
    })?;
    Ok(())
}

fn write_new(path: &Path, contents: &str) -> Result<(), Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

pub fn init_ca(directory: &Path) -> Result<PathBuf, Error> {
    if let Some(parent) = directory.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let key = KeyPair::generate()?;
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params
        .distinguished_name
        .push(DnType::CommonName, "Firecrab private root CA");
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.not_before = OffsetDateTime::now_utc() - Duration::minutes(5);
    params.not_after = OffsetDateTime::now_utc() + Duration::days(3650);
    let certificate = params.self_signed(&key)?;
    new_directory(directory)?;
    write_new(&directory.join("ca.key"), &key.serialize_pem())?;
    let cert_path = directory.join("ca.crt");
    write_new(&cert_path, &certificate.pem())?;
    Ok(cert_path)
}

pub fn issue_host_cert(
    directory: &Path,
    id: Uuid,
    names: Vec<String>,
    days: u32,
) -> Result<TlsPaths, Error> {
    if id.is_nil() || !(1..=3650).contains(&days) {
        return Err(Error(
            "provide a non-nil host UUID and 1..=3650 validity days".into(),
        ));
    }
    let ca = root_certificate(&directory.join("ca.crt"))?;
    let key_pem = read_private(&directory.join("ca.key"))?;
    let key = KeyPair::from_pem(
        std::str::from_utf8(&key_pem).map_err(|_| Error("invalid CA key PEM".into()))?,
    )?;
    if parse(&ca)?.public_key().raw != key.subject_public_key_info() {
        return Err(Error("CA certificate and signing key do not match".into()));
    }
    let issuer = Issuer::from_ca_cert_der(&ca, key)?;
    let now = OffsetDateTime::now_utc();
    let mut params = CertificateParams::new(names)?;
    params
        .distinguished_name
        .push(DnType::CommonName, id.to_string());
    params.subject_alt_names.push(SanType::URI(
        format!("{HOST_URI_PREFIX}{id}")
            .try_into()
            .map_err(|_| Error("invalid host URI".into()))?,
    ));
    params.not_before = now - Duration::minutes(5);
    params.not_after = now + Duration::days(i64::from(days));
    if params.not_after.unix_timestamp() > parse(&ca)?.validity().not_after.timestamp() {
        return Err(Error(
            "host certificate cannot outlive its CA; choose fewer validity days".into(),
        ));
    }
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let key = KeyPair::generate()?;
    let cert = params.signed_by(&key, &issuer)?;
    let hosts = directory.join("hosts");
    if !hosts.exists() {
        new_directory(&hosts)?;
    }
    let output = hosts.join(id.to_string());
    new_directory(&output)?;
    let paths = TlsPaths {
        cert: output.join("host.crt"),
        key: output.join("host.key"),
        ca: directory.join("ca.crt"),
    };
    write_new(&paths.key, &key.serialize_pem())?;
    write_new(&paths.cert, &cert.pem())?;
    Ok(paths)
}
