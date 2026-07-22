//! Per-VM TAP device lifecycle: create a persistent TAP via `/dev/net/tun`,
//! attach it to the shared bridge with an ownership alias, and tear it down
//! again on stop. The interface name is never taken from the caller — both
//! this module and the API side derive it deterministically from `vm_id`
//! via `firecrab_helper_protocol::network::tap_name`.

use std::io;
use std::os::fd::AsRawFd;

use firecrab_helper_protocol::network::tap_name;
use futures_util::TryStreamExt;
use rtnetlink::packet_route::link::{LinkAttribute, LinkMessage};
use rtnetlink::{Handle, LinkUnspec, new_connection};
use thiserror::Error;
use uuid::Uuid;

use crate::bridge::BRIDGE_NAME;

/// Linux's `IFNAMSIZ`: the fixed size of `ifreq.ifr_name`.
const IFNAMSIZ: usize = 16;

/// Failure modes for creating, attaching, or removing a VM's TAP device.
#[derive(Debug, Error)]
pub enum TapError {
    /// Couldn't open `/dev/net/tun`.
    #[error("failed to open /dev/net/tun")]
    OpenTun(#[source] io::Error),
    /// The `TUNSETIFF` ioctl failed.
    #[error("failed to create TAP device {name}")]
    Create {
        /// The interface name that failed to create.
        name: String,
        #[source]
        source: io::Error,
    },
    /// The `TUNSETPERSIST` ioctl failed.
    #[error("failed to make TAP device {name} persistent")]
    Persist {
        /// The interface name that failed to persist.
        name: String,
        #[source]
        source: io::Error,
    },
    /// Couldn't open the rtnetlink connection.
    #[error("failed to open rtnetlink connection")]
    Connection(#[source] io::Error),
    /// An rtnetlink request failed.
    #[error("rtnetlink operation failed")]
    Netlink(#[source] rtnetlink::Error),
    /// The shared bridge hasn't been created yet.
    #[error("bridge {BRIDGE_NAME} does not exist yet")]
    MissingBridge,
    /// The TAP device vanished between being created and being configured.
    #[error("TAP device {0} disappeared while it was being configured")]
    MissingAfterCreate(String),
    /// The device with this name isn't the one this `vm_id` created —
    /// refuses to delete an interface it doesn't recognize as its own.
    #[error("TAP device {name} is not owned by vm {vm_id} (found alias {alias:?})")]
    OwnershipMismatch {
        /// The interface name in question.
        name: String,
        /// The VM that requested the delete.
        vm_id: Uuid,
        /// The interface's actual alias, if any.
        alias: Option<String>,
    },
}

/// The `IFLA_IFALIAS` value that marks a TAP as owned by `vm_id`, checked
/// again before every delete so a name collision (or a stale/foreign
/// interface reusing the name) is never torn down by mistake.
fn owner_alias(vm_id: Uuid) -> String {
    format!("firecrab:{vm_id}")
}

/// Creates `vm_id`'s TAP device, attaches it to the shared bridge, tags it
/// with an ownership alias, and brings it up.
///
/// A name collision with an existing link is only reused if that link is
/// already ours (alias matches) — this never silently takes over a foreign
/// or stale-from-before-this-check interface. And if anything after
/// creating a fresh device fails (missing bridge, the device vanishing, the
/// alias/attach/up call itself), that fresh device is deleted again before
/// returning the error, so a partial failure never leaves an orphaned TAP.
pub async fn create_tap(vm_id: Uuid) -> Result<(), TapError> {
    let name = tap_name(vm_id);
    let (connection, handle, _) = new_connection().map_err(TapError::Connection)?;
    tokio::spawn(connection);

    let created_now = match find_link(&handle, &name).await? {
        Some(existing) => {
            let alias = link_alias(&existing);
            if alias.as_deref() != Some(owner_alias(vm_id).as_str()) {
                return Err(TapError::OwnershipMismatch { name, vm_id, alias });
            }
            false
        }
        None => {
            create_persistent_device(&name)?;
            true
        }
    };

    let result = attach_and_configure(&handle, &name, vm_id).await;
    if result.is_err() && created_now {
        // Best-effort: a cleanup failure here must not mask the original
        // error, but a freshly-created device that we failed to finish
        // configuring must not survive as an orphan either.
        if let Ok(Some(link)) = find_link(&handle, &name).await {
            let _ = handle.link().del(link.header.index).execute().await;
        }
    }
    result
}

/// Looks up `name` and the shared bridge, then attaches/aliases/brings it up.
async fn attach_and_configure(handle: &Handle, name: &str, vm_id: Uuid) -> Result<(), TapError> {
    let bridge_index = find_link(handle, BRIDGE_NAME)
        .await?
        .ok_or(TapError::MissingBridge)?
        .header
        .index;
    let tap_index = find_link(handle, name)
        .await?
        .ok_or_else(|| TapError::MissingAfterCreate(name.to_owned()))?
        .header
        .index;

    handle
        .link()
        .set(
            LinkUnspec::new_with_index(tap_index)
                .append_extra_attribute(LinkAttribute::IfAlias(owner_alias(vm_id)))
                .controller(bridge_index)
                .up()
                .build(),
        )
        .execute()
        .await
        .map_err(TapError::Netlink)
}

/// Removes `vm_id`'s TAP device after confirming its alias still matches —
/// a no-op if the device is already gone (stop/delete may race a prior
/// cleanup or a start that never got this far).
pub async fn delete_tap(vm_id: Uuid) -> Result<(), TapError> {
    let name = tap_name(vm_id);
    let (connection, handle, _) = new_connection().map_err(TapError::Connection)?;
    tokio::spawn(connection);

    let Some(link) = find_link(&handle, &name).await? else {
        return Ok(());
    };

    let alias = link_alias(&link);
    if alias.as_deref() != Some(owner_alias(vm_id).as_str()) {
        return Err(TapError::OwnershipMismatch { name, vm_id, alias });
    }

    handle
        .link()
        .del(link.header.index)
        .execute()
        .await
        .map_err(TapError::Netlink)
}

fn link_alias(link: &LinkMessage) -> Option<String> {
    link.attributes
        .iter()
        .find_map(|attribute| match attribute {
            LinkAttribute::IfAlias(alias) => Some(alias.clone()),
            _ => None,
        })
}

async fn find_link(handle: &Handle, name: &str) -> Result<Option<LinkMessage>, TapError> {
    let mut links = handle.link().get().match_name(name.to_owned()).execute();
    match links.try_next().await {
        Ok(link) => Ok(link),
        // A get-by-name answers ENODEV when the link does not exist.
        Err(rtnetlink::Error::NetlinkError(message)) if message.raw_code() == -libc::ENODEV => {
            Ok(None)
        }
        Err(error) => Err(TapError::Netlink(error)),
    }
}

/// Argument struct for the raw `TUNSETIFF`/`TUNSETPERSIST` ioctls on
/// `/dev/net/tun`. Only `ifr_name` and the `ifr_flags` slot at the front of
/// the kernel's `ifr_ifru` union are meaningful here; the rest is padding
/// sized to match the kernel's `struct ifreq` so the ioctl doesn't read
/// past this buffer.
#[repr(C)]
struct IfReq {
    ifr_name: [u8; IFNAMSIZ],
    ifr_flags: libc::c_short,
    _reserved: [u8; 22],
}

impl IfReq {
    fn for_tap(name: &str) -> Self {
        let mut ifr_name = [0_u8; IFNAMSIZ];
        // Name is at most 15 chars (see tap_name's doc comment), leaving
        // room for the NUL the kernel expects to terminate ifr_name.
        ifr_name[..name.len()].copy_from_slice(name.as_bytes());
        Self {
            ifr_name,
            ifr_flags: (libc::IFF_TAP | libc::IFF_NO_PI) as libc::c_short,
            _reserved: [0; 22],
        }
    }
}

/// Creates `name` as a TAP device via `/dev/net/tun`, marking it persistent
/// so it survives past this process closing its file descriptor (Firecracker
/// opens the device itself by name once it starts; this helper doesn't hand
/// its fd off to anyone).
fn create_persistent_device(name: &str) -> Result<(), TapError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .map_err(TapError::OpenTun)?;
    let fd = file.as_raw_fd();

    let mut request = IfReq::for_tap(name);
    // SAFETY: `request` is a valid, properly sized buffer for the duration
    // of the call; `fd` is a valid, open file descriptor for /dev/net/tun.
    let result = unsafe { libc::ioctl(fd, libc::TUNSETIFF, &mut request) };
    if result < 0 {
        return Err(TapError::Create {
            name: name.to_owned(),
            source: io::Error::last_os_error(),
        });
    }

    // SAFETY: as above; the third argument is a plain integer (1 = enable),
    // not a pointer.
    let result = unsafe { libc::ioctl(fd, libc::TUNSETPERSIST, 1) };
    if result < 0 {
        return Err(TapError::Persist {
            name: name.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_alias_embeds_the_vm_id() {
        let vm_id = Uuid::from_u128(0x1234);
        assert_eq!(owner_alias(vm_id), format!("firecrab:{vm_id}"));
    }

    #[test]
    fn ifreq_encodes_the_name_and_tap_no_pi_flags() {
        let request = IfReq::for_tap("fct0123456789ab");
        assert_eq!(&request.ifr_name[..15], b"fct0123456789ab");
        assert_eq!(request.ifr_name[15], 0, "name must stay NUL-terminated");
        assert_eq!(
            request.ifr_flags as i32,
            libc::IFF_TAP | libc::IFF_NO_PI,
            "must request a TAP (not TUN) device without packet info framing"
        );
    }
}
