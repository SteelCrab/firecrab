//! Named `firecrab-api` endpoints stored in the user's firecrab directory.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::api_client::{ApiClient, ApiError};

/// Filename below the resolved firecrab user directory.
const CONFIG_FILE: &str = "config.toml";

/// Saved-host operations exposed by `firecrab host`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Save a named firecrab host.
    Add {
        /// Short name used by `--host` and `host use`.
        name: String,
        /// API base URL, for example `https://firecrab.example.com:5523`.
        url: String,
        /// Make this host current immediately.
        #[arg(long)]
        r#use: bool,
    },
    /// List saved hosts and mark the current one.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Make a saved host current.
    Use {
        /// Name of a saved host.
        name: String,
    },
    /// Resolve a host and verify that its API is reachable.
    Show {
        /// Saved name; the current host or loopback default when omitted.
        name: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove a saved host.
    Remove {
        /// Name of a saved host.
        name: String,
    },
}

/// A named host entry in `~/firecrab/config.toml`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HostEntry {
    /// Normalized HTTP(S) API base URL.
    url: String,
}

/// Complete user-level CLI configuration.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HostsConfig {
    /// Name used when neither `--api`, `FIRECRAB_API`, nor `--host` is set.
    current_host: Option<String>,
    /// Stable ordering keeps both human output and the TOML file predictable.
    #[serde(default)]
    hosts: BTreeMap<String, HostEntry>,
}

/// A host name and URL selected from the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedHost {
    /// Saved name, absent only for the built-in loopback default.
    pub name: Option<String>,
    /// Normalized HTTP(S) API base URL.
    pub url: String,
}

/// Configuration and host-selection failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No supported home-directory variable is available.
    #[error("cannot locate the user home directory; set HOME, USERPROFILE, or FIRECRAB_CONFIG_DIR")]
    NoHomeDirectory,
    /// Reading the configuration failed.
    #[error("failed to read {path}: {source}")]
    Read {
        /// File that could not be read.
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// TOML parsing failed.
    #[error("failed to parse {path}: {source}")]
    Parse {
        /// File containing invalid TOML.
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    /// TOML serialization failed.
    #[error("failed to serialize {path}: {source}")]
    Serialize {
        /// Destination file.
        path: PathBuf,
        #[source]
        source: toml::ser::Error,
    },
    /// Creating or atomically replacing the configuration failed.
    #[error("failed to write {path}: {source}")]
    Write {
        /// Destination file.
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Acquiring the sibling configuration lock failed.
    #[error("failed to lock {path}: {source}")]
    Lock {
        /// Lock file that could not be acquired.
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A host name is unsafe or ambiguous on the command line.
    #[error("invalid host name {0:?}; use letters, digits, '.', '_' or '-'")]
    InvalidName(String),
    /// The endpoint is not a usable HTTP(S) base URL.
    #[error("invalid API URL {url:?}: {reason}")]
    InvalidUrl {
        /// Rejected input.
        url: String,
        /// Actionable validation reason.
        reason: String,
    },
    /// The requested name is absent from the configuration.
    #[error("no saved host named {0:?}; run `firecrab host list`")]
    UnknownHost(String),
    /// `host add` would silently replace an existing endpoint.
    #[error("host {0:?} already exists; remove it before adding a replacement")]
    DuplicateHost(String),
    /// The configuration points at a deleted or manually renamed entry.
    #[error("current_host {0:?} has no matching entry in the config")]
    MissingCurrentHost(String),
    /// The selected endpoint did not answer its status probe.
    #[error("failed to connect to host {name:?} at {url}: {source}")]
    Connection {
        /// Saved name or `default` for loopback.
        name: String,
        /// Resolved endpoint.
        url: String,
        #[source]
        source: ApiError,
    },
}

impl HostsConfig {
    /// Loads TOML from `path`, treating a missing file as an empty config.
    fn load(path: &Path) -> Result<Self, Error> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| Error::Parse {
                path: path.to_owned(),
                source,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Read {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Serializes and atomically replaces `path` from a sibling temporary file.
    fn save(&self, path: &Path) -> Result<(), Error> {
        let parent = path.parent().ok_or_else(|| Error::Write {
            path: path.to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "config path has no parent directory",
            ),
        })?;
        fs::create_dir_all(parent).map_err(|source| Error::Write {
            path: path.to_owned(),
            source,
        })?;
        let text = toml::to_string_pretty(self).map_err(|source| Error::Serialize {
            path: path.to_owned(),
            source,
        })?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| Error::Write {
                path: path.to_owned(),
                source,
            })?;
        temporary
            .write_all(text.as_bytes())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|source| Error::Write {
                path: path.to_owned(),
                source,
            })?;
        temporary.persist(path).map_err(|error| Error::Write {
            path: path.to_owned(),
            source: error.error,
        })?;
        Ok(())
    }
}

/// Acquires an exclusive lock separate from the atomically replaced config file.
fn lock_config(path: &Path) -> Result<fs::File, Error> {
    let parent = path.parent().ok_or_else(|| Error::Write {
        path: path.to_owned(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "config path has no parent directory",
        ),
    })?;
    fs::create_dir_all(parent).map_err(|source| Error::Write {
        path: path.to_owned(),
        source,
    })?;
    let mut filename = path
        .file_name()
        .ok_or_else(|| Error::Write {
            path: path.to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "config path has no filename",
            ),
        })?
        .to_os_string();
    filename.push(".lock");
    let lock_path = parent.join(filename);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| Error::Lock {
            path: lock_path.clone(),
            source,
        })?;
    lock.lock_exclusive().map_err(|source| Error::Lock {
        path: lock_path,
        source,
    })?;
    Ok(lock)
}

/// Loads, changes, and persists one configuration while holding its file lock.
fn change_config<T>(
    path: &Path,
    change: impl FnOnce(&mut HostsConfig) -> Result<T, Error>,
) -> Result<T, Error> {
    let lock = lock_config(path)?;
    let mut config = HostsConfig::load(path)?;
    let result = change(&mut config)?;
    config.save(path)?;
    drop(lock);
    Ok(result)
}

/// Filters empty environment values before they become relative paths.
fn non_empty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|item| !item.is_empty())
}

/// Pure platform-aware half of [`config_path`].
fn config_path_from(
    override_dir: Option<OsString>,
    home: Option<OsString>,
    user_profile: Option<OsString>,
    windows: bool,
) -> Option<PathBuf> {
    if let Some(directory) = non_empty(override_dir) {
        return Some(PathBuf::from(directory).join(CONFIG_FILE));
    }
    let home = if windows {
        non_empty(user_profile).or_else(|| non_empty(home))
    } else {
        non_empty(home).or_else(|| non_empty(user_profile))
    }?;
    Some(PathBuf::from(home).join("firecrab").join(CONFIG_FILE))
}

/// Returns the configured path, normally `~/firecrab/config.toml`.
pub fn config_path() -> Result<PathBuf, Error> {
    config_path_from(
        std::env::var_os("FIRECRAB_CONFIG_DIR"),
        std::env::var_os("HOME"),
        std::env::var_os("USERPROFILE"),
        cfg!(windows),
    )
    .ok_or(Error::NoHomeDirectory)
}

/// Validates and normalizes an endpoint from a flag, environment, or config.
pub fn normalize_url(input: &str) -> Result<String, Error> {
    let input = input.trim();
    let parsed = reqwest::Url::parse(input).map_err(|source| Error::InvalidUrl {
        url: input.to_owned(),
        reason: source.to_string(),
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::InvalidUrl {
            url: input.to_owned(),
            reason: "expected an http:// or https:// URL".to_owned(),
        });
    }
    if parsed.host_str().is_none() {
        return Err(Error::InvalidUrl {
            url: input.to_owned(),
            reason: "host is missing".to_owned(),
        });
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Error::InvalidUrl {
            url: input.to_owned(),
            reason: "embedded credentials are not allowed".to_owned(),
        });
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(Error::InvalidUrl {
            url: input.to_owned(),
            reason: "query strings and fragments are not allowed".to_owned(),
        });
    }
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

/// Restricts names to the portable characters accepted on every shell.
fn validate_name(name: &str) -> Result<(), Error> {
    if !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        return Ok(());
    }
    Err(Error::InvalidName(name.to_owned()))
}

/// Resolves a requested or current host from one explicit config path.
fn selected_at(path: &Path, requested: Option<&str>) -> Result<Option<SelectedHost>, Error> {
    let config = HostsConfig::load(path)?;
    let name = match requested {
        Some(name) => name,
        None => match config.current_host.as_deref() {
            Some(name) => name,
            None => return Ok(None),
        },
    };
    let entry = config.hosts.get(name).ok_or_else(|| match requested {
        Some(_) => Error::UnknownHost(name.to_owned()),
        None => Error::MissingCurrentHost(name.to_owned()),
    })?;
    Ok(Some(SelectedHost {
        name: Some(name.to_owned()),
        url: normalize_url(&entry.url)?,
    }))
}

/// Resolves an explicit or current saved host without applying API overrides.
pub fn selected(requested: Option<&str>) -> Result<Option<SelectedHost>, Error> {
    match config_path() {
        Ok(path) => selected_at(&path, requested),
        Err(Error::NoHomeDirectory) if requested.is_none() => Ok(None),
        Err(error) => Err(error),
    }
}

/// Adds one entry and reports whether it became current.
fn add_at(path: &Path, name: &str, url: &str, make_current: bool) -> Result<bool, Error> {
    validate_name(name)?;
    let url = normalize_url(url)?;
    change_config(path, |config| {
        if config.hosts.contains_key(name) {
            return Err(Error::DuplicateHost(name.to_owned()));
        }
        let becomes_current = make_current || config.hosts.is_empty();
        config.hosts.insert(name.to_owned(), HostEntry { url });
        if becomes_current {
            config.current_host = Some(name.to_owned());
        }
        Ok(becomes_current)
    })
}

/// Changes the current host in one explicit config file.
fn use_at(path: &Path, name: &str) -> Result<(), Error> {
    change_config(path, |config| {
        if !config.hosts.contains_key(name) {
            return Err(Error::UnknownHost(name.to_owned()));
        }
        config.current_host = Some(name.to_owned());
        Ok(())
    })
}

/// Removes one entry and reports whether it had been current.
fn remove_at(path: &Path, name: &str) -> Result<bool, Error> {
    change_config(path, |config| {
        if config.hosts.remove(name).is_none() {
            return Err(Error::UnknownHost(name.to_owned()));
        }
        let was_current = config.current_host.as_deref() == Some(name);
        if was_current {
            config.current_host = None;
        }
        Ok(was_current)
    })
}

/// Formats the stable tabular output of `host list`.
fn format_list(config: &HostsConfig) -> String {
    if config.hosts.is_empty() {
        return "no saved hosts (run `firecrab host add`)\n".to_owned();
    }
    let mut output = String::from("NAME\tURL\tCURRENT\n");
    for (name, entry) in &config.hosts {
        let current = config.current_host.as_deref() == Some(name.as_str());
        output.push_str(&format!(
            "{name}\t{}\t{}\n",
            entry.url,
            if current { "*" } else { "" }
        ));
    }
    output
}

/// Probes and prints one fully resolved host.
fn show_host(selected: SelectedHost, json: bool) -> Result<(), Error> {
    let status = ApiClient::new(selected.url.clone()).get_host_status();
    let name = selected.name.unwrap_or_else(|| "default".to_owned());
    let host = status.map_err(|source| Error::Connection {
        name: name.clone(),
        url: selected.url.clone(),
        source,
    })?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": name,
                "url": selected.url,
                "reachable": true,
                "host": host,
            }))
            .expect("host status always serializes")
        );
    } else {
        println!(
            "{name}\t{}\treachable\tload {:.2}\tmemory {}/{} MiB available\tuptime {}s",
            selected.url,
            host.load_average_1m,
            host.memory_available_mib,
            host.memory_total_mib,
            host.uptime_seconds
        );
    }
    Ok(())
}

/// Resolves `host show` while allowing its positional name to be authoritative.
fn selection_for_show_at(
    path: &Path,
    positional_name: Option<&str>,
    api: Option<&str>,
    global_host: Option<&str>,
) -> Result<SelectedHost, Error> {
    if let Some(name) = positional_name {
        return selected_at(path, Some(name))?.ok_or_else(|| Error::UnknownHost(name.to_owned()));
    }
    if let Some(api) = api {
        return Ok(SelectedHost {
            name: None,
            url: normalize_url(api)?,
        });
    }
    if let Ok(environment) = std::env::var("FIRECRAB_API")
        && !environment.is_empty()
    {
        return Ok(SelectedHost {
            name: None,
            url: normalize_url(&environment)?,
        });
    }
    Ok(selected_at(path, global_host)?.unwrap_or(SelectedHost {
        name: None,
        url: crate::api_client::DEFAULT_API_BASE.to_owned(),
    }))
}

/// Resolves `host show` without requiring a config directory for direct endpoints.
fn selection_for_show(
    positional_name: Option<&str>,
    api: Option<&str>,
    global_host: Option<&str>,
) -> Result<SelectedHost, Error> {
    if positional_name.is_some() {
        return selection_for_show_at(&config_path()?, positional_name, api, global_host);
    }
    if let Some(api) = api {
        return Ok(SelectedHost {
            name: None,
            url: normalize_url(api)?,
        });
    }
    if let Ok(environment) = std::env::var("FIRECRAB_API")
        && !environment.is_empty()
    {
        return Ok(SelectedHost {
            name: None,
            url: normalize_url(&environment)?,
        });
    }
    match config_path() {
        Ok(path) => selection_for_show_at(&path, None, None, global_host),
        Err(Error::NoHomeDirectory) if global_host.is_none() => Ok(SelectedHost {
            name: None,
            url: crate::api_client::DEFAULT_API_BASE.to_owned(),
        }),
        Err(error) => Err(error),
    }
}

/// Executes one `firecrab host` command.
pub fn run(command: Command, api: Option<&str>, global_host: Option<&str>) -> Result<(), Error> {
    if let Command::Show { name, json } = command {
        return show_host(selection_for_show(name.as_deref(), api, global_host)?, json);
    }
    let path = config_path()?;
    run_at(&path, command, api, global_host)
}

/// Executes one host command against an explicit config path.
fn run_at(
    path: &Path,
    command: Command,
    api: Option<&str>,
    global_host: Option<&str>,
) -> Result<(), Error> {
    match command {
        Command::Add { name, url, r#use } => {
            let current = add_at(path, &name, &url, r#use)?;
            println!(
                "added {name} -> {}{}",
                normalize_url(&url)?,
                if current { " (current)" } else { "" }
            );
            Ok(())
        }
        Command::List { json } => {
            let config = HostsConfig::load(path)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&config).expect("host config always serializes")
                );
            } else {
                print!("{}", format_list(&config));
            }
            Ok(())
        }
        Command::Use { name } => {
            use_at(path, &name)?;
            println!("current host is now {name}");
            Ok(())
        }
        Command::Show { name, json } => {
            let selected = selection_for_show_at(path, name.as_deref(), api, global_host)?;
            show_host(selected, json)
        }
        Command::Remove { name } => {
            let was_current = remove_at(path, &name)?;
            println!(
                "removed {name}{}",
                if was_current {
                    " (current host cleared)"
                } else {
                    ""
                }
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;

    fn temp_config() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("firecrab").join(CONFIG_FILE);
        (directory, path)
    }

    #[test]
    fn config_path_uses_the_requested_cross_platform_home() {
        let unix = config_path_from(None, Some("/home/alice".into()), None, false).unwrap();
        assert_eq!(unix, Path::new("/home/alice/firecrab/config.toml"));

        let windows = config_path_from(
            None,
            Some("ignored".into()),
            Some(r"C:\Users\alice".into()),
            true,
        )
        .unwrap();
        assert_eq!(windows, Path::new(r"C:\Users\alice/firecrab/config.toml"));
    }

    #[test]
    fn config_path_override_is_platform_independent() {
        let path = config_path_from(Some("portable".into()), None, None, true).unwrap();
        assert_eq!(path, Path::new("portable/config.toml"));
    }

    #[test]
    fn missing_config_loads_as_empty() {
        let (_directory, path) = temp_config();
        assert_eq!(HostsConfig::load(&path).unwrap(), HostsConfig::default());
    }

    #[test]
    fn config_round_trip_is_toml_and_creates_parent_directory() {
        let (_directory, path) = temp_config();
        add_at(&path, "prod", "https://prod.example:5523/", false).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("current_host = \"prod\""));
        assert!(text.contains("[hosts.prod]"));
        assert_eq!(
            selected_at(&path, None).unwrap().unwrap(),
            SelectedHost {
                name: Some("prod".to_owned()),
                url: "https://prod.example:5523".to_owned(),
            }
        );
    }

    #[test]
    fn invalid_toml_is_reported_instead_of_ignored() {
        let (_directory, path) = temp_config();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "current_host = [").unwrap();
        assert!(matches!(HostsConfig::load(&path), Err(Error::Parse { .. })));
    }

    #[test]
    fn unknown_toml_fields_are_rejected_before_a_mutation_can_drop_them() {
        let (_directory, path) = temp_config();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"
future_setting = "keep"
current_host = "prod"

[hosts.prod]
url = "https://prod.example:5523"
"#,
        )
        .unwrap();

        assert!(matches!(use_at(&path, "prod"), Err(Error::Parse { .. })));
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("future_setting = \"keep\"")
        );
    }

    #[test]
    fn url_validation_accepts_http_paths_and_rejects_unsafe_bases() {
        assert_eq!(
            normalize_url(" https://example.test/firecrab/ ").unwrap(),
            "https://example.test/firecrab"
        );
        for invalid in [
            "example.test:5523",
            "file:///tmp/api",
            "https://user:secret@example.test",
            "https://example.test?q=1",
        ] {
            assert!(matches!(
                normalize_url(invalid),
                Err(Error::InvalidUrl { .. })
            ));
        }
    }

    #[test]
    fn host_names_are_restricted_to_portable_characters() {
        for valid in ["prod", "seoul-1", "lab.a", "lab_a"] {
            assert!(validate_name(valid).is_ok());
        }
        for invalid in ["", "two words", "slash/name", "한글"] {
            assert!(matches!(validate_name(invalid), Err(Error::InvalidName(_))));
        }
    }

    #[test]
    fn first_add_becomes_current_and_explicit_use_switches_it() {
        let (_directory, path) = temp_config();
        assert!(add_at(&path, "first", "http://first:5523", false).unwrap());
        assert!(!add_at(&path, "second", "http://second:5523", false).unwrap());
        use_at(&path, "second").unwrap();
        assert_eq!(
            selected_at(&path, None).unwrap().unwrap().name.as_deref(),
            Some("second")
        );
    }

    #[test]
    fn concurrent_mutations_preserve_every_profile() {
        let (_directory, path) = temp_config();
        let workers = 12;
        let ready = Arc::new(Barrier::new(workers + 1));
        let mut joins = Vec::new();
        for number in 0..workers {
            let path = path.clone();
            let ready = Arc::clone(&ready);
            joins.push(thread::spawn(move || {
                ready.wait();
                add_at(
                    &path,
                    &format!("host-{number}"),
                    &format!("http://127.0.0.1:{}", 20_000 + number),
                    false,
                )
            }));
        }
        ready.wait();
        for join in joins {
            join.join().unwrap().unwrap();
        }

        assert_eq!(HostsConfig::load(&path).unwrap().hosts.len(), workers);
    }

    #[test]
    fn add_rejects_duplicates_and_selection_rejects_unknown_names() {
        let (_directory, path) = temp_config();
        add_at(&path, "prod", "http://prod:5523", false).unwrap();
        assert!(matches!(
            add_at(&path, "prod", "http://other:5523", false),
            Err(Error::DuplicateHost(name)) if name == "prod"
        ));
        assert!(matches!(
            selected_at(&path, Some("missing")),
            Err(Error::UnknownHost(name)) if name == "missing"
        ));
    }

    #[test]
    fn removing_current_clears_selection_without_removing_other_hosts() {
        let (_directory, path) = temp_config();
        add_at(&path, "first", "http://first:5523", false).unwrap();
        add_at(&path, "second", "http://second:5523", false).unwrap();
        assert!(remove_at(&path, "first").unwrap());
        assert!(selected_at(&path, None).unwrap().is_none());
        assert!(selected_at(&path, Some("second")).unwrap().is_some());
    }

    #[test]
    fn list_is_sorted_and_marks_current() {
        let config = HostsConfig {
            current_host: Some("zeta".to_owned()),
            hosts: BTreeMap::from([
                (
                    "zeta".to_owned(),
                    HostEntry {
                        url: "http://zeta:5523".to_owned(),
                    },
                ),
                (
                    "alpha".to_owned(),
                    HostEntry {
                        url: "http://alpha:5523".to_owned(),
                    },
                ),
            ]),
        };
        assert_eq!(
            format_list(&config),
            "NAME\tURL\tCURRENT\nalpha\thttp://alpha:5523\t\nzeta\thttp://zeta:5523\t*\n"
        );
    }

    #[test]
    fn command_dispatch_persists_lists_switches_and_removes_hosts() {
        let (_directory, path) = temp_config();
        run_at(
            &path,
            Command::Add {
                name: "first".to_owned(),
                url: "http://first:5523/".to_owned(),
                r#use: false,
            },
            None,
            None,
        )
        .unwrap();
        run_at(
            &path,
            Command::Add {
                name: "second".to_owned(),
                url: "https://second:5523".to_owned(),
                r#use: true,
            },
            None,
            None,
        )
        .unwrap();
        run_at(&path, Command::List { json: false }, None, None).unwrap();
        run_at(&path, Command::List { json: true }, None, None).unwrap();
        run_at(
            &path,
            Command::Use {
                name: "first".to_owned(),
            },
            None,
            None,
        )
        .unwrap();
        run_at(
            &path,
            Command::Remove {
                name: "second".to_owned(),
            },
            None,
            None,
        )
        .unwrap();
        run_at(
            &path,
            Command::Remove {
                name: "first".to_owned(),
            },
            None,
            None,
        )
        .unwrap();
        assert_eq!(HostsConfig::load(&path).unwrap(), HostsConfig::default());
    }

    #[test]
    fn show_host_probes_the_selected_api() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /api/host "));
            let body = r#"{"loadAverage1m":0.5,"memoryTotalMib":2048,"memoryAvailableMib":1024,"diskTotalGib":100,"diskAvailableGib":80,"uptimeSeconds":3600}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });

        show_host(
            SelectedHost {
                name: Some("test".to_owned()),
                url,
            },
            false,
        )
        .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn show_host_reports_name_and_url_on_connection_failure() {
        let error = show_host(
            SelectedHost {
                name: Some("offline".to_owned()),
                url: "http://127.0.0.1:1".to_owned(),
            },
            false,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::Connection { name, url, .. }
                if name == "offline" && url == "http://127.0.0.1:1"
        ));
    }

    #[test]
    fn explicit_show_name_wins_over_global_api_selection() {
        let (_directory, path) = temp_config();
        add_at(&path, "prod", "https://prod.example:5523", false).unwrap();
        assert_eq!(
            selection_for_show_at(
                &path,
                Some("prod"),
                Some("https://override.example:5523"),
                None,
            )
            .unwrap(),
            SelectedHost {
                name: Some("prod".to_owned()),
                url: "https://prod.example:5523".to_owned(),
            }
        );
    }

    #[test]
    fn direct_show_selection_does_not_require_a_config_path() {
        assert_eq!(
            selection_for_show(None, Some("https://direct.example:5523/"), None).unwrap(),
            SelectedHost {
                name: None,
                url: "https://direct.example:5523".to_owned(),
            }
        );
    }
}
