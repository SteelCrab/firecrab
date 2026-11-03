//! Observe the installed Linux API/helper inputs without evaluating systemd or shell text.
//!
//! Values stay on the native host. Only a complete configuration digest crosses the broker.

use super::{invalid, journal::Snapshot};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::Path,
};

const FILE_LIMIT: u64 = 1024 * 1024;
const MAX_DROP_INS: usize = 128;

fn protected_file(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.file_type().is_file()
        && metadata.nlink() == 1
        && metadata.uid() == nix::unistd::Uid::current().as_raw()
        && metadata.mode() & 0o022 == 0
        && metadata.len() <= FILE_LIMIT
}

fn file(path: &Path) -> io::Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if !protected_file(&metadata) {
        return Err(invalid(
            "host configuration requires protected bounded regular files",
        ));
    }
    let mut bytes = Vec::new();
    let opened = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    let actual = opened.metadata()?;
    if !protected_file(&actual) || actual.dev() != metadata.dev() || actual.ino() != metadata.ino()
    {
        return Err(invalid(
            "host configuration changed while opening its original file",
        ));
    }
    opened.take(FILE_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(invalid("host configuration exceeds its read limit"));
    }
    Ok(Some(format!("{:x}", Sha256::digest(bytes))))
}

/// This projection includes original physical directories and every installed unit drop-in.
pub(super) fn snapshot(home: &Path, unitdir: &Path) -> io::Result<Snapshot> {
    use std::os::unix::fs::MetadataExt;
    let units = fs::canonicalize(unitdir)?;
    let metadata = fs::symlink_metadata(&units)?;
    if !metadata.is_dir()
        || metadata.uid() != nix::unistd::Uid::current().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(invalid("host unit directory is not protected"));
    }
    let mut inputs = BTreeMap::new();
    inputs.insert("api.env".to_owned(), file(&home.join("api.env"))?);
    for unit in crate::service::env::UNITS {
        inputs.insert(unit.to_owned(), file(&units.join(unit))?);
        let directory = units.join(format!("{unit}.d"));
        let entries = match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
            Ok(metadata)
                if !metadata.is_dir()
                    || metadata.file_type().is_symlink()
                    || metadata.uid() != nix::unistd::Uid::current().as_raw()
                    || metadata.mode() & 0o022 != 0 =>
            {
                return Err(invalid("host unit drop-in directory is not protected"));
            }
            Ok(_) => fs::read_dir(&directory)?,
        };
        let mut count = 0;
        for entry in entries {
            count += 1;
            if count > MAX_DROP_INS {
                return Err(invalid("host unit drop-in inventory exceeds its limit"));
            }
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| invalid("host drop-in name is not UTF-8"))?;
            if !name.ends_with(".conf") {
                continue;
            }
            if name.chars().any(char::is_control) {
                return Err(invalid("host unit drop-in inventory exceeds its limit"));
            }
            inputs.insert(format!("{unit}.d/{name}"), file(&entry.path())?);
        }
    }
    let exists = inputs.get("api.env").is_some_and(Option::is_some)
        && crate::service::env::UNITS
            .iter()
            .all(|unit| inputs.get(*unit).is_some_and(Option::is_some));
    let directory_id = super::directory_identity(&units)?;
    let projection = serde_json::to_vec(&(units, directory_id, inputs))
        .map_err(|_| invalid("invalid host configuration projection"))?;
    Ok(Snapshot {
        exists,
        busy: false,
        configuration_sha256: format!("{:x}", Sha256::digest(projection)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_service_inputs_and_drop_ins_change_the_proof_without_exposing_values() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("config");
        let units = root.path().join("units");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&units).unwrap();
        fs::write(home.join("api.env"), b"FIRECRAB_BIND_ADDR=127.0.0.1:5523\n").unwrap();
        for unit in crate::service::env::UNITS {
            fs::write(units.join(unit), b"[Service]\nExecStart=/original/binary\n").unwrap();
        }
        let original = snapshot(&home, &units).unwrap();
        assert!(original.exists);
        let drop_ins = units.join("firecrab-api.service.d");
        fs::create_dir(&drop_ins).unwrap();
        fs::write(
            drop_ins.join("20-controller.conf"),
            b"[Service]\nEnvironment=FIRECRAB_CONTROLLER_ID=fixture-only\n",
        )
        .unwrap();
        let changed = snapshot(&home, &units).unwrap();
        assert_ne!(changed.configuration_sha256, original.configuration_sha256);
        fs::remove_file(drop_ins.join("20-controller.conf")).unwrap();
        assert_eq!(
            snapshot(&home, &units).unwrap().configuration_sha256,
            original.configuration_sha256
        );
        fs::remove_file(units.join("firecrab-helper.service")).unwrap();
        assert!(!snapshot(&home, &units).unwrap().exists);
        std::os::unix::fs::symlink(home.join("api.env"), units.join("firecrab-helper.service"))
            .unwrap();
        assert!(snapshot(&home, &units).is_err());
    }
}
