use super::*;
use std::fs;

fn authority() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("pki");
    init_ca(&directory).unwrap();
    (root, directory)
}

#[test]
fn issues_matching_uuid_certificate_and_never_overwrites_keys() {
    let (_root, directory) = authority();
    let id = Uuid::new_v4();
    let paths = issue_host_cert(
        &directory,
        id,
        vec!["localhost".into(), "127.0.0.1".into()],
        90,
    )
    .unwrap();
    assert_eq!(host_id(&certificates(&paths.cert).unwrap()[0]).unwrap(), id);
    paths.server_config().unwrap();
    paths.client_config().unwrap();
    let key = read_private(&paths.key).unwrap();
    assert!(issue_host_cert(&directory, id, vec![], 90).is_err());
    assert_eq!(read_private(&paths.key).unwrap(), key);
    let ca_key = read_private(&directory.join("ca.key")).unwrap();
    assert!(init_ca(&directory).is_err());
    assert_eq!(read_private(&directory.join("ca.key")).unwrap(), ca_key);
    assert!(expires_in(&paths.cert).unwrap() > 89 * 86400);
}

#[test]
fn rejects_incomplete_configuration_nil_id_and_invalid_validity() {
    assert!(TlsPaths::from_values(None, None, None).unwrap().is_none());
    assert!(TlsPaths::from_values(Some("cert".into()), None, None).is_err());
    assert!(TlsPaths::from_values(Some("".into()), Some("key".into()), Some("ca".into())).is_err());
    let (_root, directory) = authority();
    for (id, days) in [
        (Uuid::nil(), 1),
        (Uuid::new_v4(), 0),
        (Uuid::new_v4(), 3651),
    ] {
        assert!(issue_host_cert(&directory, id, vec![], days).is_err());
    }
}

#[test]
fn rejects_wrong_ca_wrong_key_and_invalid_pem() {
    let (_root, directory) = authority();
    let paths = issue_host_cert(&directory, Uuid::new_v4(), vec![], 1).unwrap();
    let other = issue_host_cert(&directory, Uuid::new_v4(), vec![], 1).unwrap();
    let wrong_key = TlsPaths {
        key: other.key,
        ..paths.clone()
    };
    assert!(wrong_key.server_config().is_err());
    let (_foreign_root, foreign) = authority();
    let wrong_ca = TlsPaths {
        ca: foreign.join("ca.crt"),
        ..paths.clone()
    };
    assert!(wrong_ca.server_config().is_err());
    fs::write(&paths.cert, "not a certificate").unwrap();
    assert!(paths.server_config().is_err());
}

#[cfg(unix)]
#[test]
fn generated_files_are_private_and_unsafe_files_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (_root, directory) = authority();
    let paths = issue_host_cert(&directory, Uuid::new_v4(), vec![], 1).unwrap();
    for file in [
        &paths.cert,
        &paths.key,
        &paths.ca,
        &directory.join("ca.key"),
    ] {
        assert_eq!(
            fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let link = directory.join("linked.key");
    symlink(&paths.key, &link).unwrap();
    assert!(read_private(&link).is_err());
    assert!(read_private(&directory).is_err());
    fs::set_permissions(&paths.key, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(paths.client_config().is_err());
}

#[test]
fn issuer_rejects_a_mismatched_ca_signing_key() {
    let (_root, directory) = authority();
    let (_foreign_root, foreign) = authority();
    fs::write(
        directory.join("ca.key"),
        read_private(&foreign.join("ca.key")).unwrap(),
    )
    .unwrap();
    assert!(
        issue_host_cert(&directory, Uuid::new_v4(), vec![], 1)
            .unwrap_err()
            .to_string()
            .contains("do not match")
    );
}

#[test]
fn rejects_inconsistent_cn_and_uri_identities() {
    use rcgen::{CertificateParams, DnType, KeyPair, SanType};
    let id = Uuid::new_v4();
    for cn in [
        "not-a-uuid".to_owned(),
        Uuid::new_v4().to_string(),
        Uuid::nil().to_string(),
    ] {
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, cn);
        params.subject_alt_names.push(SanType::URI(
            format!("{HOST_URI_PREFIX}{id}").try_into().unwrap(),
        ));
        let cert = params.self_signed(&KeyPair::generate().unwrap()).unwrap();
        assert!(host_id(cert.der()).is_err());
    }
}
