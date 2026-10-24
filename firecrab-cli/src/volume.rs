//! Named persistent data disks through the host API.

use crate::api_client::{ApiClient, ApiError};
use clap::Subcommand;
use firecrab_api_types::{
    AttachMicroBlockStorageRequest, CreateMicroBlockStorageRequest, MicroBlockStorageResponse,
    VmBlockStorageMount,
};
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List named data volumes and their exclusive attachments.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create an empty ext4 data volume.
    Create {
        name: String,
        /// Capacity in GiB.
        #[arg(long, value_name = "GIB")]
        size: u16,
        #[arg(long)]
        storage_root: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one volume by name or UUID.
    Inspect {
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Attach a volume to an inactive user MicroVM.
    Attach {
        target: String,
        #[arg(long)]
        vm: Uuid,
        #[arg(long)]
        mount_path: String,
        #[arg(long)]
        json: bool,
    },
    /// Detach from an inactive MicroVM without deleting data.
    Detach {
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Permanently delete an unattached volume and its data.
    Delete {
        target: String,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(client: &ApiClient, command: Command) -> Result<(), ApiError> {
    match command {
        Command::List { json } => {
            let volumes: Vec<MicroBlockStorageResponse> = client.get("/api/microblockstorage")?;
            if json {
                print_json(&volumes);
            } else {
                println!("ID\tNAME\tSIZE_GIB\tSTATE\tVM\tMOUNT");
                for volume in volumes {
                    print_volume(&volume, false);
                }
            }
        }
        Command::Create {
            name,
            size,
            storage_root,
            json,
        } => {
            let volume = client.post(
                "/api/microblockstorage",
                &CreateMicroBlockStorageRequest {
                    name,
                    size_gib: size,
                    storage_root,
                },
            )?;
            print_volume(&volume, json);
        }
        Command::Inspect { target, json } => {
            let id = resolve_id(client, &target)?;
            print_volume(&client.get(&format!("/api/microblockstorage/{id}"))?, json);
        }
        Command::Attach {
            target,
            vm,
            mount_path,
            json,
        } => {
            let id = resolve_id(client, &target)?;
            let volume = client.post(
                &format!("/api/microblockstorage/{id}/attach"),
                &AttachMicroBlockStorageRequest {
                    vm_id: vm,
                    mount_path,
                },
            )?;
            print_volume(&volume, json);
        }
        Command::Detach { target, json } => {
            let id = resolve_id(client, &target)?;
            print_volume(
                &client.post_empty(&format!("/api/microblockstorage/{id}/detach"))?,
                json,
            );
        }
        Command::Delete { target, json } => {
            let id = resolve_id(client, &target)?;
            client.delete(&format!("/api/microblockstorage/{id}"))?;
            if json {
                print_json(&serde_json::json!({ "deleted": id }));
            } else {
                println!("deleted volume {id}");
            }
        }
    }
    Ok(())
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("volume response serializes")
    );
}

fn print_volume(volume: &MicroBlockStorageResponse, json: bool) {
    if json {
        print_json(volume);
        return;
    }
    println!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        volume.id,
        volume.name,
        volume.size_gib,
        volume.state,
        volume
            .attached_vm_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "-".into()),
        volume.mount_path.as_deref().unwrap_or("-")
    );
}

fn resolve_id(client: &ApiClient, target: &str) -> Result<Uuid, ApiError> {
    if let Ok(id) = Uuid::parse_str(target) {
        return Ok(id);
    }
    let volumes: Vec<MicroBlockStorageResponse> = client.get("/api/microblockstorage")?;
    volumes
        .into_iter()
        .find(|volume| volume.name == target)
        .map(|volume| volume.id)
        .ok_or_else(|| ApiError::Http {
            status: 404,
            body: format!("no volume named {target:?}"),
        })
}

/// Resolves repeated --volume NAME_OR_UUID:/guest/path arguments before VM creation.
pub(crate) fn resolve_mounts(
    client: &ApiClient,
    values: &[String],
) -> Result<Vec<VmBlockStorageMount>, ApiError> {
    values
        .iter()
        .map(|value| {
            let (target, mount_path) = value
                .split_once(':')
                .filter(|(target, path)| !target.is_empty() && path.starts_with('/'))
                .ok_or_else(|| ApiError::Http {
                    status: 400,
                    body: "volume must have the form NAME_OR_UUID:/guest/path".into(),
                })?;
            Ok(VmBlockStorageMount {
                volume_id: resolve_id(client, target)?,
                mount_path: mount_path.into(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_arguments_resolve_uuid_mounts_without_contacting_the_api() {
        let client = ApiClient::new("http://127.0.0.1:1".into());
        let id = Uuid::from_u128(247);
        let mounts = resolve_mounts(&client, &[format!("{id}:/var/lib/postgresql/data")]).unwrap();
        assert_eq!(
            mounts,
            [VmBlockStorageMount {
                volume_id: id,
                mount_path: "/var/lib/postgresql/data".into(),
            }]
        );
    }

    #[test]
    fn volume_arguments_reject_missing_name_or_absolute_path() {
        let client = ApiClient::new("http://127.0.0.1:1".into());
        for input in ["data", "data:relative", ":/data"] {
            assert!(resolve_mounts(&client, &[input.into()]).is_err());
        }
    }
}
