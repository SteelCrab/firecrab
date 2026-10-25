//! Prepare broker authentication offline, before a guest can accept messages.
use super::EngineRecord;
use crate::{persistence::Store, rootfs};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::path::Path;
use uuid::Uuid;

/// Broker-local Erlang authentication file; never exposed as an application binding.
const COOKIE: &str = "/var/lib/rabbitmq/.erlang.cookie";

/// Derive a stable, domain-separated secret without extending public configuration.
fn cookie(record: &EngineRecord) -> Result<String, String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(record.password.as_bytes())
        .map_err(|_| "Could not derive Queue node authentication".to_owned())?;
    mac.update(b"firecrab-queue-erlang-cookie-v1");
    Ok(format!("{:x}", mac.finalize().into_bytes()))
}

/// Create the node cookie on the offline disk with the image user's ownership.
///
/// A guest-generated cookie can remain empty after an early power-off even when
/// the message journal was synced. Preparing it before boot also repairs that
/// empty file without replacing the VM, its data directory or its identity.
pub(crate) fn prepare_engine_disk(store: &Store, id: Uuid, image: &Path) -> Result<(), String> {
    let engine = store
        .queue_engine()
        .map_err(|_| "Could not read Queue engine")?
        .filter(|engine| engine.vm_id == id)
        .ok_or("Queue VM does not belong to the configured engine")?;
    let passwd = rootfs::run_debugfs(image, "cat /etc/passwd")
        .map_err(|_| "Could not read the broker image user")?;
    let passwd =
        std::str::from_utf8(&passwd.stdout).map_err(|_| "Could not read the broker image user")?;
    let (uid, gid) = passwd
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            if fields.next()? != "rabbitmq" {
                return None;
            }
            fields.next()?;
            Some((
                fields.next()?.parse::<u32>().ok()?,
                fields.next()?.parse::<u32>().ok()?,
            ))
        })
        .ok_or("RabbitMQ user is missing from the broker image")?;
    rootfs::write_into_image(image, COOKIE, cookie(&engine)?.as_bytes())
        .map_err(|_| "Could not prepare Queue node authentication")?;
    for field in [
        "mode 0100400".to_owned(),
        format!("uid {uid}"),
        format!("gid {gid}"),
    ] {
        rootfs::run_debugfs(image, &format!("set_inode_field {COOKIE} {field}"))
            .map_err(|_| "Could not protect Queue node authentication")?;
    }
    let metadata = rootfs::run_debugfs(image, &format!("stat {COOKIE}"))
        .map_err(|_| "Could not verify Queue node authentication permissions")?;
    let metadata = String::from_utf8_lossy(&metadata.stdout);
    let fields: Vec<_> = metadata.split_whitespace().collect();
    for (key, value) in [
        ("Mode:", "0400".to_owned()),
        ("User:", uid.to_string()),
        ("Group:", gid.to_string()),
    ] {
        if !fields.windows(2).any(|pair| pair == [key, value.as_str()]) {
            return Err("Could not protect Queue node authentication".into());
        }
    }
    Ok(())
}
