//! Host API commands for PipeStorage. Keys and cursors are URL-encoded.
use crate::api_client::ApiClient;
use clap::Subcommand;
use firecrab_api_types::{PipeStorageResponse, StorageObjectPage};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List saved S3 connections without credentials.
    List,
    /// Inspect one connection and its service references.
    Get { id: Uuid },
    /// Verify and save configuration from a private JSON file.
    Create {
        #[arg(long)]
        file: PathBuf,
    },
    /// Verify replacement settings. Omit credentials to retain existing keys.
    Update {
        id: Uuid,
        #[arg(long)]
        file: PathBuf,
    },
    /// Check ListBucket without test writes or deletes.
    Verify { id: Uuid },
    /// Disconnect without deleting S3 data. Unbind referencing services first.
    Delete { id: Uuid },
    /// List a directory page. Keys include the saved connection prefix.
    Objects {
        id: Uuid,
        #[arg(long)]
        prefix: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
    /// Stream a file to S3 (maximum 512 MiB); existing keys are protected by default.
    Upload {
        id: Uuid,
        key: String,
        file: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    /// Stream an object into a file; publish the file only after a complete download.
    Download {
        id: Uuid,
        key: String,
        file: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    /// Delete exactly one S3 object. Requires explicit confirmation.
    RemoveObject {
        id: Uuid,
        key: String,
        #[arg(long, required = true)]
        yes: bool,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Api(#[from] crate::api_client::ApiError),
    #[error("Could not read the private S3 configuration file: {0}")]
    Read(#[from] std::io::Error),
    #[error("Invalid S3 configuration JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn run(client: &ApiClient, command: Command) -> Result<(), Error> {
    let base = "/api/pipe/storage";
    match command {
        Command::List => print(&client.get::<Vec<PipeStorageResponse>>(base)?),
        Command::Get { id } => print(&client.get::<PipeStorageResponse>(&format!("{base}/{id}"))?),
        Command::Create { file } => {
            let input: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print(&client.post::<_, PipeStorageResponse>(base, &input)?);
        }
        Command::Update { id, file } => {
            let input: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print(&client.put::<_, PipeStorageResponse>(&format!("{base}/{id}"), &input)?);
        }
        Command::Verify { id } => {
            print(&client.post_empty::<serde_json::Value>(&format!("{base}/{id}/verify"))?)
        }
        Command::Delete { id } => {
            client.delete(&format!("{base}/{id}"))?;
            println!("Disconnected");
        }
        Command::Objects {
            id,
            prefix,
            cursor,
            limit,
        } => {
            let mut query = vec![("limit", limit.to_string())];
            if let Some(prefix) = prefix {
                query.push(("prefix", prefix));
            }
            if let Some(cursor) = cursor {
                query.push(("cursor", cursor));
            }
            print(
                &client
                    .get_query::<_, StorageObjectPage>(&format!("{base}/{id}/objects"), &query)?,
            );
        }
        Command::Upload {
            id,
            key,
            file,
            overwrite,
        } => {
            client.upload_object(&object_path(id, &key, overwrite), &file)?;
            println!("Uploaded");
        }
        Command::Download {
            id,
            key,
            file,
            overwrite,
        } => {
            client.download_object(&object_path(id, &key, false), &file, overwrite)?;
            println!("Downloaded");
        }
        Command::RemoveObject { id, key, .. } => {
            client.delete(&object_path(id, &key, false))?;
            println!("Deleted object");
        }
    }
    Ok(())
}

fn object_path(id: Uuid, key: &str, overwrite: bool) -> String {
    let mut query = reqwest::Url::parse("http://localhost/").expect("static URL");
    query
        .query_pairs_mut()
        .append_pair("key", key)
        .append_pair("overwrite", if overwrite { "true" } else { "false" });
    format!(
        "/api/pipe/storage/{id}/object?{}",
        query.query().expect("query set")
    )
}
fn print(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("serializable metadata")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn commands_use_the_selected_host_and_encode_keys_without_changing_the_path() {
        let cli = crate::Cli::try_parse_from([
            "firecrab",
            "--host",
            "qa",
            "pipe",
            "storage",
            "upload",
            "00000000-0000-0000-0000-000000000001",
            "team/a?b #+%/한글",
            "file.bin",
        ])
        .unwrap();
        assert_eq!(cli.host.as_deref(), Some("qa"));
        let path = object_path(Uuid::nil(), "team/a?b #+%/한글", false);
        let url = reqwest::Url::parse(&format!("http://localhost{path}")).unwrap();
        assert_eq!(url.query_pairs().next().unwrap().1, "team/a?b #+%/한글");
        assert_eq!(
            url.path(),
            "/api/pipe/storage/00000000-0000-0000-0000-000000000000/object"
        );
        assert!(
            crate::Cli::try_parse_from([
                "firecrab",
                "pipe",
                "storage",
                "remove-object",
                "00000000-0000-0000-0000-000000000001",
                "key"
            ])
            .is_err()
        );
    }
}
