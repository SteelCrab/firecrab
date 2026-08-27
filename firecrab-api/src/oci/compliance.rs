//! Compliance evidence for OCI imports.
//!
//! The importer inspects package-manager state in the merged rootfs before
//! Firecrab provisions it.  Package databases are parsed as data; no binary
//! from the imported image is executed on the host.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::image_install::Architecture;

const RPMTAG_NAME: u32 = 1000;
const RPMTAG_VERSION: u32 = 1001;
const RPMTAG_RELEASE: u32 = 1002;
const RPMTAG_EPOCH: u32 = 1003;
const RPMTAG_LICENSE: u32 = 1014;
const RPMTAG_ARCH: u32 = 1022;
const RPMTAG_SOURCERPM: u32 = 1044;

const RPM_INT32_TYPE: u32 = 4;
const RPM_STRING_TYPE: u32 = 6;
const RPM_STRING_ARRAY_TYPE: u32 = 8;
const RPM_I18NSTRING_TYPE: u32 = 9;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageRecord {
    name: String,
    version: String,
    arch: String,
    license: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PackageDb {
    Apk(PathBuf),
    Dpkg(PathBuf),
    Rpm(PathBuf),
}

impl PackageDb {
    const fn label(&self) -> &'static str {
        match self {
            Self::Apk(_) => "apk",
            Self::Dpkg(_) => "dpkg",
            Self::Rpm(_) => "rpm",
        }
    }

    fn path(&self) -> &Path {
        match self {
            Self::Apk(path) | Self::Dpkg(path) | Self::Rpm(path) => path,
        }
    }
}

/// Generated OCI SPDX evidence waiting to be attached to the registered alias.
#[derive(Debug, Clone)]
pub(super) struct GeneratedSbom {
    pub(super) bytes: Vec<u8>,
    pub(super) package_manager: &'static str,
    pub(super) package_count: usize,
}

/// Detect apk/dpkg/RPM state and build one SPDX 2.3 document.
///
/// `Ok(None)` means this image does not expose a package database we know how
/// to inspect.  The caller intentionally treats that as warning-only: an OCI
/// image can still be useful even when it is scratch/distroless/custom.
pub(super) fn generate_spdx(
    rootfs: &Path,
    alias: &str,
    image_version: &str,
    architecture: Architecture,
) -> Result<Option<GeneratedSbom>, String> {
    let Some(database) = detect_package_db(rootfs) else {
        return Ok(None);
    };

    let mut packages = match &database {
        PackageDb::Apk(path) => parse_apk(&read_text(path)?)?,
        PackageDb::Dpkg(path) => parse_dpkg(&read_text(path)?)?,
        PackageDb::Rpm(path) => parse_rpm_db(path)?,
    };
    packages.sort_by(|a, b| {
        (&a.name, &a.version, &a.arch).cmp(&(&b.name, &b.version, &b.arch))
    });
    packages.dedup_by(|a, b| {
        a.name == b.name && a.version == b.version && a.arch == b.arch
    });
    if packages.is_empty() {
        return Err(format!(
            "{} package database contained no installed packages: {}",
            database.label(),
            database.path().display()
        ));
    }

    let distribution = os_release_id(rootfs).unwrap_or_else(|| database.label().to_owned());
    let document = make_spdx(
        alias,
        image_version,
        architecture,
        &distribution,
        database.label(),
        &packages,
    );
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("serialize SPDX document: {error}"))?;
    bytes.push(b'\n');

    Ok(Some(GeneratedSbom {
        bytes,
        package_manager: database.label(),
        package_count: packages.len(),
    }))
}

/// Persist an OCI SBOM at the stable path consumed by the follow-up API/UI.
pub(super) fn write_spdx_bundle(
    image_root: &Path,
    alias: &str,
    architecture: Architecture,
    document: &GeneratedSbom,
) -> io::Result<PathBuf> {
    let directory = bundle_directory(image_root, alias, architecture);
    fs::create_dir_all(&directory)?;
    let output = directory.join("sbom.spdx.json");
    let temporary = directory.join(format!(".sbom.spdx.json.{}.tmp", std::process::id()));
    fs::write(&temporary, &document.bytes)?;
    fs::rename(&temporary, &output)?;
    Ok(output)
}

pub(super) fn remove_bundle(image_root: &Path, alias: &str, architecture: Architecture) {
    let _ = fs::remove_dir_all(bundle_directory(image_root, alias, architecture));
}

fn bundle_directory(image_root: &Path, alias: &str, architecture: Architecture) -> PathBuf {
    image_root
        .join("compliance")
        .join(format!("{alias}-{}", architecture.as_str()))
}

fn detect_package_db(rootfs: &Path) -> Option<PackageDb> {
    safe_regular_file(rootfs, "lib/apk/db/installed")
        .map(PackageDb::Apk)
        .or_else(|| safe_regular_file(rootfs, "var/lib/dpkg/status").map(PackageDb::Dpkg))
        // Fedora/RHEL-family rpm >= 4.16 stores the SQLite database here.
        .or_else(|| {
            safe_regular_file(rootfs, "usr/lib/sysimage/rpm/rpmdb.sqlite").map(PackageDb::Rpm)
        })
        // Rocky container roots commonly keep the database directly here.
        .or_else(|| safe_regular_file(rootfs, "var/lib/rpm/rpmdb.sqlite").map(PackageDb::Rpm))
}

/// Package metadata must not follow an imported symlink out of the merged tree.
fn safe_regular_file(rootfs: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = rootfs.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            return None;
        };
        path.push(component);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() {
            return None;
        }
    }
    fs::metadata(&path).ok()?.is_file().then_some(path)
}

fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn parse_apk(text: &str) -> Result<Vec<PackageRecord>, String> {
    let mut packages = Vec::new();
    for paragraph in text.split("\n\n") {
        let mut name = None;
        let mut version = None;
        let mut arch = None;
        let mut license = None;
        let mut source = None;
        for line in paragraph.lines() {
            let bytes = line.as_bytes();
            if bytes.len() < 2 || bytes[1] != b':' {
                continue;
            }
            let value = line[2..].trim();
            match bytes[0] {
                b'P' => name = Some(value.to_owned()),
                b'V' => version = Some(value.to_owned()),
                b'A' => arch = Some(value.to_owned()),
                b'L' => license = Some(value.to_owned()),
                b'o' => source = Some(value.to_owned()),
                _ => {}
            }
        }
        if let (Some(name), Some(version)) = (name, version) {
            packages.push(PackageRecord {
                source: source.unwrap_or_else(|| name.clone()),
                name,
                version,
                arch: arch.unwrap_or_else(|| "unknown".to_owned()),
                license: license.unwrap_or_default(),
            });
        }
    }
    Ok(packages)
}

fn parse_dpkg(text: &str) -> Result<Vec<PackageRecord>, String> {
    let mut packages = Vec::new();
    for paragraph in text.split("\n\n") {
        let mut fields = std::collections::HashMap::<String, String>::new();
        let mut current = None::<String>;
        for line in paragraph.lines() {
            if line.starts_with([' ', '\t']) {
                if let Some(key) = current.as_ref() {
                    fields
                        .entry(key.clone())
                        .and_modify(|value| {
                            value.push(' ');
                            value.push_str(line.trim());
                        });
                }
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.to_owned();
            fields.insert(key.clone(), value.trim().to_owned());
            current = Some(key);
        }
        if fields.get("Status").map(String::as_str) != Some("install ok installed") {
            continue;
        }
        let Some(name) = fields.get("Package").cloned() else {
            continue;
        };
        let Some(version) = fields.get("Version").cloned() else {
            continue;
        };
        let source = fields
            .get("Source")
            .and_then(|value| value.split_whitespace().next())
            .unwrap_or(&name)
            .to_owned();
        packages.push(PackageRecord {
            name,
            version,
            arch: fields
                .get("Architecture")
                .cloned()
                .unwrap_or_else(|| "unknown".to_owned()),
            license: String::new(),
            source,
        });
    }
    Ok(packages)
}

fn parse_rpm_db(path: &Path) -> Result<Vec<PackageRecord>, String> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(path, flags)
        .map_err(|error| format!("open RPM database {}: {error}", path.display()))?;
    let mut statement = connection
        .prepare("SELECT blob FROM Packages ORDER BY hnum")
        .map_err(|error| format!("query RPM database {}: {error}", path.display()))?;
    let rows = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| format!("read RPM database {}: {error}", path.display()))?;

    let mut packages = Vec::new();
    for row in rows {
        let blob = row.map_err(|error| format!("read RPM package header: {error}"))?;
        packages.push(parse_rpm_header(&blob)?);
    }
    Ok(packages)
}

#[derive(Debug, Clone, Copy)]
struct RpmIndex {
    kind: u32,
    offset: usize,
    count: u32,
}

fn parse_rpm_header(blob: &[u8]) -> Result<PackageRecord, String> {
    let magic = [0x8e, 0xad, 0xe8];
    let start = blob
        .windows(magic.len())
        .take(32)
        .position(|candidate| candidate == magic)
        .ok_or_else(|| "RPM database row is missing a package header magic".to_owned())?;
    if blob.len() < start + 16 {
        return Err("RPM package header is truncated".to_owned());
    }
    let index_count = be_u32(blob, start + 8)? as usize;
    let store_size = be_u32(blob, start + 12)? as usize;
    let indexes_start = start + 16;
    let store_start = indexes_start
        .checked_add(index_count.checked_mul(16).ok_or("RPM header index overflow")?)
        .ok_or("RPM header offset overflow")?;
    let store_end = store_start
        .checked_add(store_size)
        .ok_or("RPM header store overflow")?;
    if store_end > blob.len() {
        return Err("RPM package header data store is truncated".to_owned());
    }
    let store = &blob[store_start..store_end];

    let mut indexes = std::collections::HashMap::<u32, RpmIndex>::new();
    for index in 0..index_count {
        let offset = indexes_start + index * 16;
        let tag = be_u32(blob, offset)?;
        indexes.insert(
            tag,
            RpmIndex {
                kind: be_u32(blob, offset + 4)?,
                offset: be_u32(blob, offset + 8)? as usize,
                count: be_u32(blob, offset + 12)?,
            },
        );
    }

    let name = rpm_string(store, indexes.get(&RPMTAG_NAME))?
        .ok_or_else(|| "RPM package header has no name".to_owned())?;
    let version = rpm_string(store, indexes.get(&RPMTAG_VERSION))?
        .ok_or_else(|| format!("RPM package {name} has no version"))?;
    let release = rpm_string(store, indexes.get(&RPMTAG_RELEASE))?.unwrap_or_default();
    let epoch = rpm_u32(store, indexes.get(&RPMTAG_EPOCH))?.unwrap_or(0);
    let mut complete_version = if release.is_empty() {
        version
    } else {
        format!("{version}-{release}")
    };
    if epoch > 0 {
        complete_version = format!("{epoch}:{complete_version}");
    }

    Ok(PackageRecord {
        name,
        version: complete_version,
        arch: rpm_string(store, indexes.get(&RPMTAG_ARCH))?
            .unwrap_or_else(|| "unknown".to_owned()),
        license: rpm_string(store, indexes.get(&RPMTAG_LICENSE))?.unwrap_or_default(),
        source: rpm_string(store, indexes.get(&RPMTAG_SOURCERPM))?.unwrap_or_default(),
    })
}

fn be_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "RPM header integer is truncated".to_owned())?;
    Ok(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
}

fn rpm_string(store: &[u8], index: Option<&RpmIndex>) -> Result<Option<String>, String> {
    let Some(index) = index else {
        return Ok(None);
    };
    if !matches!(
        index.kind,
        RPM_STRING_TYPE | RPM_STRING_ARRAY_TYPE | RPM_I18NSTRING_TYPE
    ) || index.count == 0
    {
        return Ok(None);
    }
    let bytes = store
        .get(index.offset..)
        .ok_or_else(|| "RPM string offset is outside the data store".to_owned())?;
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    let value = std::str::from_utf8(&bytes[..end])
        .map_err(|error| format!("RPM string is not UTF-8: {error}"))?;
    Ok(Some(value.to_owned()))
}

fn rpm_u32(store: &[u8], index: Option<&RpmIndex>) -> Result<Option<u32>, String> {
    let Some(index) = index else {
        return Ok(None);
    };
    if index.kind != RPM_INT32_TYPE || index.count == 0 {
        return Ok(None);
    }
    let bytes = store
        .get(index.offset..index.offset + 4)
        .ok_or_else(|| "RPM integer offset is outside the data store".to_owned())?;
    Ok(Some(u32::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3],
    ])))
}

fn os_release_id(rootfs: &Path) -> Option<String> {
    let path = safe_regular_file(rootfs, "etc/os-release")?;
    let text = fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key == "ID").then(|| value.trim().trim_matches(['\"', '\'']).to_owned())
    })
}

fn make_spdx(
    alias: &str,
    image_version: &str,
    architecture: Architecture,
    distribution: &str,
    package_manager: &str,
    packages: &[PackageRecord],
) -> Value {
    let fingerprint = package_fingerprint(packages);
    let created = rfc3339_now();
    let image_id = "SPDXRef-OCIImage";
    let mut spdx_packages = vec![json!({
        "name": alias,
        "SPDXID": image_id,
        "versionInfo": image_version,
        "downloadLocation": "NOASSERTION",
        "filesAnalyzed": false,
        "licenseConcluded": "NOASSERTION",
        "licenseDeclared": "NOASSERTION",
        "copyrightText": "NOASSERTION",
        "comment": format!(
            "distribution={distribution}; architecture={}; package-manager={package_manager}",
            architecture.as_str()
        )
    })];
    let mut relationships = vec![json!({
        "spdxElementId": "SPDXRef-DOCUMENT",
        "relationshipType": "DESCRIBES",
        "relatedSpdxElement": image_id
    })];

    for package in packages {
        let package_id = stable_package_id(package);
        let mut entry = json!({
            "name": package.name,
            "SPDXID": package_id,
            "versionInfo": package.version,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": false,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "copyrightText": "NOASSERTION"
        });
        let mut comments = Vec::new();
        if !package.license.is_empty() {
            comments.push(format!("package-manager-license={}", package.license));
        }
        if !package.source.is_empty() {
            comments.push(format!("source-package={}", package.source));
        }
        if !comments.is_empty() {
            entry
                .as_object_mut()
                .expect("package entry must be an object")
                .insert("comment".to_owned(), Value::String(comments.join("; ")));
        }
        spdx_packages.push(entry);
        relationships.push(json!({
            "spdxElementId": image_id,
            "relationshipType": "CONTAINS",
            "relatedSpdxElement": package_id
        }));
    }

    json!({
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": format!("FireCrab OCI {alias} {}", architecture.as_str()),
        "documentNamespace": format!(
            "https://firecrab.dev/spdx/oci/{alias}/{}/{fingerprint}",
            architecture.as_str()
        ),
        "creationInfo": {
            "created": created,
            "creators": ["Tool: firecrab OCI import"]
        },
        "packages": spdx_packages,
        "relationships": relationships
    })
}

fn stable_package_id(package: &PackageRecord) -> String {
    let seed = format!("{}\0{}\0{}", package.name, package.version, package.arch);
    let digest = format!("{:x}", Sha256::digest(seed.as_bytes()));
    let clean: String = package
        .name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect();
    format!(
        "SPDXRef-Package-{}-{}",
        clean.trim_matches('-'),
        &digest[..16]
    )
}

fn package_fingerprint(packages: &[PackageRecord]) -> String {
    let mut digest = Sha256::new();
    for package in packages {
        digest.update(package.name.as_bytes());
        digest.update([0]);
        digest.update(package.version.as_bytes());
        digest.update([0]);
        digest.update(package.arch.as_bytes());
        digest.update([0]);
        digest.update(package.license.as_bytes());
        digest.update([0]);
        digest.update(package.source.as_bytes());
        digest.update([0xff]);
    }
    format!("{digest:x}")
}

fn rfc3339_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = (seconds / 86_400) as i64;
    let day_seconds = seconds % 86_400;
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

// Howard Hinnant's civil-from-days transform, with Unix epoch day 0 as input.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096)
            / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apk_database_generates_spdx() {
        let directory = tempfile::tempdir().unwrap();
        let db = directory.path().join("lib/apk/db");
        fs::create_dir_all(&db).unwrap();
        fs::write(
            db.join("installed"),
            "P:busybox\nV:1.37.0-r9\nA:x86_64\nL:GPL-2.0-only\no:busybox\n\nP:ssl_client\nV:1.37.0-r9\nA:x86_64\no:busybox\n",
        )
        .unwrap();
        fs::create_dir_all(directory.path().join("etc")).unwrap();
        fs::write(directory.path().join("etc/os-release"), "ID=alpine\n").unwrap();

        let generated = generate_spdx(
            directory.path(),
            "alpine-test",
            "latest",
            Architecture::X86_64,
        )
        .unwrap()
        .unwrap();
        assert_eq!(generated.package_manager, "apk");
        assert_eq!(generated.package_count, 2);
        let json: Value = serde_json::from_slice(&generated.bytes).unwrap();
        assert_eq!(json["spdxVersion"], "SPDX-2.3");
        assert_eq!(json["packages"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn dpkg_database_ignores_removed_packages() {
        let directory = tempfile::tempdir().unwrap();
        let db = directory.path().join("var/lib/dpkg");
        fs::create_dir_all(&db).unwrap();
        fs::write(
            db.join("status"),
            "Package: curl\nStatus: install ok installed\nArchitecture: amd64\nVersion: 8.5.0-2ubuntu10\nSource: curl (8.5.0-2ubuntu10)\n\nPackage: old\nStatus: deinstall ok config-files\nArchitecture: amd64\nVersion: 1\n",
        )
        .unwrap();
        let generated = generate_spdx(
            directory.path(),
            "ubuntu-test",
            "24.04",
            Architecture::X86_64,
        )
        .unwrap()
        .unwrap();
        assert_eq!(generated.package_count, 1);
    }

    #[test]
    fn unknown_package_manager_is_warning_only_signal() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            generate_spdx(
                directory.path(),
                "scratch",
                "latest",
                Architecture::X86_64,
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn bundle_path_matches_follow_up_api_contract() {
        let directory = tempfile::tempdir().unwrap();
        let document = GeneratedSbom {
            bytes: b"{}\n".to_vec(),
            package_manager: "apk",
            package_count: 1,
        };
        let path = write_spdx_bundle(
            directory.path(),
            "nginx-1.27",
            Architecture::X86_64,
            &document,
        )
        .unwrap();
        assert_eq!(
            path,
            directory
                .path()
                .join("compliance/nginx-1.27-x86_64/sbom.spdx.json")
        );
        assert_eq!(fs::read(path).unwrap(), b"{}\n");
    }

    #[test]
    fn rpm_header_parser_reads_core_tags() {
        let blob = rpm_fixture(&[
            (RPMTAG_NAME, RPM_STRING_TYPE, b"bash\0".to_vec(), 1),
            (RPMTAG_VERSION, RPM_STRING_TYPE, b"5.1.8\0".to_vec(), 1),
            (RPMTAG_RELEASE, RPM_STRING_TYPE, b"9.el9\0".to_vec(), 1),
            (RPMTAG_ARCH, RPM_STRING_TYPE, b"x86_64\0".to_vec(), 1),
            (RPMTAG_LICENSE, RPM_STRING_TYPE, b"GPLv3+\0".to_vec(), 1),
            (
                RPMTAG_SOURCERPM,
                RPM_STRING_TYPE,
                b"bash-5.1.8-9.el9.src.rpm\0".to_vec(),
                1,
            ),
            (RPMTAG_EPOCH, RPM_INT32_TYPE, 0_u32.to_be_bytes().to_vec(), 1),
        ]);
        let package = parse_rpm_header(&blob).unwrap();
        assert_eq!(package.name, "bash");
        assert_eq!(package.version, "5.1.8-9.el9");
        assert_eq!(package.arch, "x86_64");
        assert_eq!(package.license, "GPLv3+");
        assert_eq!(package.source, "bash-5.1.8-9.el9.src.rpm");
    }

    fn rpm_fixture(entries: &[(u32, u32, Vec<u8>, u32)]) -> Vec<u8> {
        let mut indexes = Vec::new();
        let mut store = Vec::new();
        for (tag, kind, value, count) in entries {
            indexes.extend_from_slice(&tag.to_be_bytes());
            indexes.extend_from_slice(&kind.to_be_bytes());
            indexes.extend_from_slice(&(store.len() as u32).to_be_bytes());
            indexes.extend_from_slice(&count.to_be_bytes());
            store.extend_from_slice(value);
        }
        let mut header = vec![0x8e, 0xad, 0xe8, 1, 0, 0, 0, 0];
        header.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        header.extend_from_slice(&(store.len() as u32).to_be_bytes());
        header.extend_from_slice(&indexes);
        header.extend_from_slice(&store);
        header
    }
}
