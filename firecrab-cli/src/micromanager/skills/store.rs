//! Host copy of imported official skills, shared by every MicroVM on this controller.
//! Each skill keeps one version: VMs target names, and their installed copy stays until the next sync.
use super::super::resource::{self, Error};
use super::meta::License;
use super::source::{self, CatalogSkill, MAX_FILE, Source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StoredFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub git_sha: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    pub name: String,
    pub description: String,
    pub commit: String,
    pub license: License,
    pub digest: String,
    pub files: Vec<StoredFile>,
    pub imported_at: u64,
}

pub struct Store {
    root: PathBuf,
}

fn digest(files: &[StoredFile]) -> String {
    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut hasher = Sha256::new();
    for file in sorted {
        hasher.update(format!("{}\0{}\n", file.path, file.sha256));
    }
    format!("{:x}", hasher.finalize())
}

/// Reason codes the dashboard turns into copy; `None` means the skill may be imported.
fn reason(skill: &CatalogSkill) -> Option<&str> {
    match skill.license {
        License::Proprietary => Some("proprietary"),
        License::Unknown => Some("unknown-license"),
        License::Apache2 => skill.problem.as_deref(),
    }
}

impl Store {
    pub fn new(home: &Path) -> Self {
        Self {
            root: home.join("runtime/skills"),
        }
    }

    fn index(&self) -> PathBuf {
        self.root.join("imported.json")
    }

    fn skill_dir(&self, name: &str) -> PathBuf {
        self.root.join("store").join(name)
    }

    pub fn imported(&self) -> Result<Vec<Imported>, Error> {
        Ok(resource::read(&self.index())?.unwrap_or_default())
    }

    /// Download one skill at the current official commit and swap it in as a whole folder.
    pub fn import(&self, source: &Source, name: &str) -> Result<Imported, Error> {
        let catalog = source.catalog(&self.root.join("catalog"))?;
        let skill = catalog
            .skills
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| Error::Invalid("This skill is not in the official store".into()))?;
        if skill.license != License::Apache2 {
            return Err(Error::Invalid(
                "This skill's license does not allow Firecrab to copy it".into(),
            ));
        }
        if skill.problem.is_some() {
            return Err(Error::Invalid(
                "This skill has unsupported files or is too large".into(),
            ));
        }
        let store = self.root.join("store");
        let staging = store.join(format!(".staging-{}", uuid::Uuid::new_v4()));
        let written = self.download(source, &catalog.commit, skill, &staging);
        let files = match written {
            Ok(files) => files,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        let target = self.skill_dir(name);
        let old = store.join(format!(".old-{}", uuid::Uuid::new_v4()));
        if target.exists() {
            fs::rename(&target, &old)?;
        }
        fs::rename(&staging, &target)?;
        if old.exists() {
            fs::remove_dir_all(&old)?;
        }
        let imported = Imported {
            name: name.to_owned(),
            description: skill.description.clone(),
            commit: catalog.commit.clone(),
            license: skill.license,
            digest: digest(&files),
            files,
            imported_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        };
        let mut index = self.imported()?;
        index.retain(|i| i.name != name);
        index.push(imported.clone());
        index.sort_by(|a, b| a.name.cmp(&b.name));
        resource::write(&self.index(), &index)?;
        Ok(imported)
    }

    fn download(
        &self,
        source: &Source,
        commit: &str,
        skill: &CatalogSkill,
        staging: &Path,
    ) -> Result<Vec<StoredFile>, Error> {
        let mut files = Vec::new();
        for file in &skill.files {
            if !source::valid_relative(&file.path) {
                return Err(Error::Invalid("Invalid skill file path".into()));
            }
            let bytes = source.file(
                commit,
                &format!("skills/{}/{}", skill.name, file.path),
                MAX_FILE,
            )?;
            if bytes.len() as u64 != file.size {
                return Err(Error::Invalid(
                    "A skill file changed while it was downloaded".into(),
                ));
            }
            let path = staging.join(&file.path);
            fs::create_dir_all(path.parent().unwrap_or(staging))?;
            fs::write(&path, &bytes)?;
            files.push(StoredFile {
                path: file.path.clone(),
                size: file.size,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                git_sha: file.git_sha.clone(),
            });
        }
        Ok(files)
    }

    /// A tar of `<name>/<path>` files plus a `<name>/.firecrab-skill` ownership marker.
    pub fn archive(&self, names: &[String]) -> Result<Vec<u8>, Error> {
        let imported = self.imported()?;
        let mut builder = tar::Builder::new(Vec::new());
        let mut append = |path: String, data: &[u8]| -> Result<(), Error> {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            builder.append_data(&mut header, path, data)?;
            Ok(())
        };
        for name in names {
            let Some(skill) = imported.iter().find(|i| &i.name == name) else {
                continue;
            };
            let marker = json!({"name":skill.name,"commit":skill.commit,"digest":skill.digest});
            append(
                format!("{name}/.firecrab-skill"),
                marker.to_string().as_bytes(),
            )?;
            for file in &skill.files {
                let bytes = fs::read(self.skill_dir(name).join(&file.path))?;
                append(format!("{name}/{}", file.path), &bytes)?;
            }
        }
        Ok(builder.into_inner()?)
    }

    /// Catalog rows for the dashboard. A GitHub failure still lists what was imported.
    pub fn view(&self, source: &Source) -> Result<Value, Error> {
        let imported = self.imported()?;
        let stamp = |name: &str| {
            imported
                .iter()
                .find(|i| i.name == name)
                .map(|i| json!({"commit":i.commit,"digest":i.digest}))
        };
        let (commit, error, mut skills) = match source.catalog(&self.root.join("catalog")) {
            Ok(catalog) => {
                let rows: Vec<Value> = catalog
                    .skills
                    .iter()
                    .map(|skill| {
                        let current: BTreeSet<_> =
                            skill.files.iter().map(|f| (&f.path, &f.git_sha)).collect();
                        let update =
                            imported
                                .iter()
                                .find(|i| i.name == skill.name)
                                .is_some_and(|i| {
                                    i.files
                                        .iter()
                                        .map(|f| (&f.path, &f.git_sha))
                                        .collect::<BTreeSet<_>>()
                                        != current
                                });
                        json!({
                            "name": skill.name,
                            "description": skill.description,
                            "license": skill.license,
                            "importable": reason(skill).is_none(),
                            "reason": reason(skill),
                            "files": skill.files.iter().map(|f| &f.path).collect::<Vec<_>>(),
                            "preview": skill.preview,
                            "imported": stamp(&skill.name),
                            "updateAvailable": update,
                        })
                    })
                    .collect();
                (Some(catalog.commit.clone()), None, rows)
            }
            Err(error) => (None, Some(error.to_string()), Vec::new()),
        };
        for skill in &imported {
            if skills.iter().any(|s| s["name"] == skill.name) {
                continue;
            }
            let manifest =
                fs::read(self.skill_dir(&skill.name).join("SKILL.md")).unwrap_or_default();
            let manifest = String::from_utf8_lossy(&manifest);
            skills.push(json!({
                "name": skill.name,
                "description": skill.description,
                "license": skill.license,
                "importable": false,
                "reason": null,
                "files": skill.files.iter().map(|f| &f.path).collect::<Vec<_>>(),
                "preview": source::utf8_prefix(&manifest, 2048),
                "imported": stamp(&skill.name),
                "updateAvailable": false,
            }));
        }
        skills.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!({
            "repository": source::REPOSITORY,
            "commit": commit,
            "error": error,
            "skills": skills,
        }))
    }
}

#[cfg(test)]
mod tests;
