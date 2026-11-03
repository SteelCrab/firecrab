use super::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

pub(crate) const SHA: &str = "ca45fc496a3436251f191354091d557da31c15b5";
pub(crate) const APACHE: &str = "\n                                 Apache License\n                           Version 2.0, January 2004\n";
pub(crate) const PROPRIETARY: &str = "© 2025 Anthropic, PBC. All rights reserved.\n";

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// A tiny GitHub stand-in: exact path match, everything else is 404.
pub(crate) struct Fixture {
    pub url: String,
    routes: Arc<Mutex<HashMap<String, Reply>>>,
    calls: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Fixture {
    pub(crate) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let routes: Arc<Mutex<HashMap<String, Reply>>> = Arc::default();
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (table, seen, done) = (routes.clone(), calls.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap_or(0) == 0 {
                    continue;
                }
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                }
                drop(reader);
                let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                seen.lock().unwrap().push(path.clone());
                let reply = table.lock().unwrap().get(&path).cloned().unwrap_or(Reply {
                    status: 404,
                    headers: vec![],
                    body: b"{\"message\":\"Not Found\"}".to_vec(),
                });
                let mut head = format!(
                    "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.body.len()
                );
                for (k, v) in &reply.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&reply.body);
            }
        });
        Self {
            url,
            routes,
            calls,
            stop,
            worker: Some(worker),
        }
    }
    pub(crate) fn set(&self, path: &str, status: u16, headers: &[(&str, &str)], body: &[u8]) {
        self.routes.lock().unwrap().insert(
            path.into(),
            Reply {
                status,
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).into(), (*v).into()))
                    .collect(),
                body: body.to_vec(),
            },
        );
    }
    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    pub(crate) fn source(&self) -> Source {
        Source {
            api: self.url.clone(),
            raw: self.url.clone(),
        }
    }
    /// Publish `files` (repository path, content) plus `special` raw tree entries at `sha`.
    pub(crate) fn publish(&self, sha: &str, files: &[(&str, &str)], special: &[Value]) {
        self.set(
            &format!("/repos/{REPOSITORY}/commits/main"),
            200,
            &[],
            json!({"sha": sha}).to_string().as_bytes(),
        );
        let mut tree = vec![json!({"path":"skills","mode":"040000","type":"tree","sha":"t"})];
        for (path, content) in files {
            let digest = format!("{:x}", Sha256::digest(content.as_bytes()));
            tree.push(json!({"path":path,"mode":"100644","type":"blob","size":content.len(),"sha":&digest[..40]}));
            self.set(
                &format!("/{REPOSITORY}/{sha}/{path}"),
                200,
                &[],
                content.as_bytes(),
            );
        }
        tree.extend(special.iter().cloned());
        self.set(
            &format!("/repos/{REPOSITORY}/git/trees/{sha}?recursive=1"),
            200,
            &[],
            json!({"sha": sha, "truncated": false, "tree": tree})
                .to_string()
                .as_bytes(),
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.worker.take().unwrap().join();
    }
}

pub(crate) fn skill_md(name: &str, license: Option<&str>) -> String {
    let license = license
        .map(|l| format!("license: {l}\n"))
        .unwrap_or_default();
    format!("---\nname: {name}\ndescription: The {name} skill.\n{license}---\n# {name}\n")
}

/// `webapp-testing` (Apache, with a script) and `pdf` (Proprietary).
pub(crate) fn standard(fixture: &Fixture, sha: &str, script: &str) {
    let webapp = skill_md("webapp-testing", Some("Complete terms in LICENSE.txt"));
    let pdf = skill_md("pdf", Some("Proprietary. LICENSE.txt has complete terms"));
    fixture.publish(
        sha,
        &[
            ("skills/webapp-testing/SKILL.md", &webapp),
            ("skills/webapp-testing/LICENSE.txt", APACHE),
            ("skills/webapp-testing/scripts/run.py", script),
            ("skills/pdf/SKILL.md", &pdf),
            ("skills/pdf/LICENSE.txt", PROPRIETARY),
        ],
        &[],
    );
}

#[test]
fn catalog_lists_skills_with_license_and_files() {
    let fixture = Fixture::start();
    standard(&fixture, SHA, "print('run')\n");
    let cache = tempfile::tempdir().unwrap();
    let catalog = fixture.source().catalog(cache.path()).unwrap();
    assert_eq!(catalog.repository, "anthropics/skills");
    assert_eq!(catalog.commit, SHA);
    let names: Vec<_> = catalog.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["pdf", "webapp-testing"]);
    assert_eq!(catalog.skills[0].license, License::Proprietary);
    let webapp = &catalog.skills[1];
    assert_eq!(webapp.license, License::Apache2);
    assert_eq!(webapp.description, "The webapp-testing skill.");
    assert_eq!(webapp.problem, None);
    let mut paths: Vec<_> = webapp.files.iter().map(|f| f.path.as_str()).collect();
    paths.sort();
    assert_eq!(paths, ["LICENSE.txt", "SKILL.md", "scripts/run.py"]);
    assert!(webapp.preview.starts_with("---\nname: webapp-testing"));
}

#[test]
fn catalog_marks_symlinks_submodules_and_traversal_unsupported() {
    for special in [
        json!({"path":"skills/x/link","mode":"120000","type":"blob","size":4,"sha":"s"}),
        json!({"path":"skills/x/vendor","mode":"160000","type":"commit","sha":"s"}),
        json!({"path":"skills/x/../y","mode":"100644","type":"blob","size":1,"sha":"s"}),
    ] {
        let fixture = Fixture::start();
        let md = skill_md("x", None);
        fixture.publish(
            SHA,
            &[("skills/x/SKILL.md", &md), ("skills/x/LICENSE.txt", APACHE)],
            &[special],
        );
        let cache = tempfile::tempdir().unwrap();
        let catalog = fixture.source().catalog(cache.path()).unwrap();
        assert_eq!(
            catalog.skills[0].problem.as_deref(),
            Some("unsupported-files")
        );
    }
}

#[test]
fn catalog_marks_oversized_skill() {
    let fixture = Fixture::start();
    let md = skill_md("big", None);
    fixture.publish(
        SHA,
        &[("skills/big/SKILL.md", &md), ("skills/big/LICENSE.txt", APACHE)],
        &[json!({"path":"skills/big/blob.bin","mode":"100644","type":"blob","size":2*1024*1024+1,"sha":"s"})],
    );
    let cache = tempfile::tempdir().unwrap();
    let catalog = fixture.source().catalog(cache.path()).unwrap();
    assert_eq!(catalog.skills[0].problem.as_deref(), Some("too-large"));
}

#[test]
fn catalog_is_cached_per_commit() {
    let fixture = Fixture::start();
    standard(&fixture, SHA, "print('run')\n");
    let cache = tempfile::tempdir().unwrap();
    fixture.source().catalog(cache.path()).unwrap();
    let before = fixture.calls().len();
    let again = fixture.source().catalog(cache.path()).unwrap();
    assert_eq!(again.commit, SHA);
    assert_eq!(
        fixture.calls()[before..],
        [format!("/repos/{REPOSITORY}/commits/main")]
    );
}

#[test]
fn rate_limit_has_clear_error() {
    let fixture = Fixture::start();
    fixture.set(
        &format!("/repos/{REPOSITORY}/commits/main"),
        403,
        &[("x-ratelimit-remaining", "0")],
        b"{}",
    );
    let cache = tempfile::tempdir().unwrap();
    let error = fixture.source().catalog(cache.path()).err().unwrap();
    assert!(
        error
            .to_string()
            .contains("GitHub API rate limit reached. Try again later."),
        "{error}"
    );
}

#[test]
fn truncated_tree_is_rejected() {
    let fixture = Fixture::start();
    standard(&fixture, SHA, "x");
    fixture.set(
        &format!("/repos/{REPOSITORY}/git/trees/{SHA}?recursive=1"),
        200,
        &[],
        json!({"sha":SHA,"truncated":true,"tree":[]})
            .to_string()
            .as_bytes(),
    );
    let cache = tempfile::tempdir().unwrap();
    assert!(fixture.source().catalog(cache.path()).is_err());
}

#[test]
fn file_rejects_body_over_limit() {
    let fixture = Fixture::start();
    fixture.set(
        &format!("/{REPOSITORY}/{SHA}/skills/a/f"),
        200,
        &[],
        b"12345678901",
    );
    assert!(fixture.source().file(SHA, "skills/a/f", 10).is_err());
    assert_eq!(
        fixture.source().file(SHA, "skills/a/f", 11).unwrap(),
        b"12345678901"
    );
}

#[test]
#[ignore = "reaches github.com"]
fn official_catalog_is_readable_live() {
    let cache = tempfile::tempdir().unwrap();
    let catalog = Source::official().catalog(cache.path()).unwrap();
    let find = |name: &str| catalog.skills.iter().find(|s| s.name == name).unwrap();
    assert_eq!(find("pdf").license, License::Proprietary);
    assert_eq!(find("webapp-testing").license, License::Apache2);
    assert_eq!(find("webapp-testing").problem, None);
    assert_eq!(find("doc-coauthoring").license, License::Unknown);
    println!(
        "{} skills at {}: {:?}",
        catalog.skills.len(),
        &catalog.commit[..7],
        catalog
            .skills
            .iter()
            .map(|s| (s.name.as_str(), s.license, s.problem.as_deref()))
            .collect::<Vec<_>>()
    );
}
