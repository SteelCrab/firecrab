//! Guest `sshd` service script and config drop-in.

/// Guest service that runs `sshd -D` after first-boot packages.
pub const GUEST_SSHD_SERVICE: &str = "/etc/firecrab/services.d/sshd";

/// Drop-in loaded by OpenSSH 8+; the service also passes `-o` flags.
pub const GUEST_SSHD_DROPIN: &str = "/etc/ssh/sshd_config.d/50-firecrab.conf";

/// Guest `services.d/sshd` body. Host keys are already on disk; `-A` fills
/// any extra types. Distroless images without a binary exit 0.
///
/// An OCI import extracts the image tree unprivileged, so guest paths keep
/// the host user's uid. sshd rejects both of the directories that matters
/// for: it exits at startup when the privilege-separation directory is not
/// root-owned, and StrictModes refuses the key when `/root` is not.
pub fn sshd_service_script() -> String {
    r#"#!/etc/firecrab/busybox sh
# Firecrab: start sshd with key-only root login (issue #181).
BB=/etc/firecrab/busybox
$BB mkdir -p /run/sshd /root/.ssh
if [ ! -x /usr/sbin/sshd ]; then
  echo "FIRECRAB_SSHD skipped: no /usr/sbin/sshd" >/dev/console
  exit 0
fi
# An unprivileged import left these owned by the host user; sshd refuses to
# start on a privsep directory it does not own, and refuses the key under a
# home directory it does not own.
for dir in /var/lib/empty /var/empty /run/sshd; do
  [ -d "$dir" ] || continue
  $BB chown 0:0 "$dir" 2>/dev/null
  $BB chmod go-w "$dir" 2>/dev/null
done
$BB chown 0:0 /root /root/.ssh /root/.ssh/authorized_keys 2>/dev/null
$BB chmod 700 /root/.ssh
$BB chmod 600 /root/.ssh/authorized_keys 2>/dev/null
/usr/bin/ssh-keygen -A >/dev/null 2>&1
# Keep authentication records out of the interactive serial console.
previous_umask=$(umask)
umask 077
log_dir=/var/log/firecrab
log_file="$log_dir/sshd.log"
if ! $BB mkdir -p "$log_dir" || ! $BB touch "$log_file"; then
  echo "FIRECRAB_SSHD failed: cannot create authentication log" >&2
  exit 1
fi
$BB chown 0:0 "$log_dir" "$log_file" || exit 1
$BB chmod 700 "$log_dir" || exit 1
$BB chmod 600 "$log_file" || exit 1
umask "$previous_umask"
exec /usr/sbin/sshd -D -E "$log_file" \
  -f /etc/ssh/sshd_config.d/50-firecrab.conf \
  -o PermitRootLogin=prohibit-password \
  -o PasswordAuthentication=no \
  -o PubkeyAuthentication=yes \
  -o AuthorizedKeysFile=/root/.ssh/authorized_keys
"#
    .to_owned()
}

/// sshd_config.d contents matching the `-o` flags.
pub fn sshd_dropin() -> &'static [u8] {
    b"PermitRootLogin prohibit-password\n\
PasswordAuthentication no\n\
PubkeyAuthentication yes\n\
AuthorizedKeysFile /root/.ssh/authorized_keys\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sshd refuses to start when its privilege-separation directory is
    /// not root-owned, and refuses the key when `/root` is not either —
    /// both are uid 1000 on a tree an unprivileged OCI import extracted.
    #[test]
    fn sshd_service_script_repairs_ownership_an_unprivileged_import_left_behind() {
        let script = sshd_service_script();
        assert!(script.contains("/var/lib/empty"), "{script}");
        assert!(script.contains("/var/empty"), "{script}");
        assert!(script.contains("chown 0:0 /root /root/.ssh"), "{script}");
        assert!(
            script.find("chown").unwrap() < script.find("exec /usr/sbin/sshd").unwrap(),
            "ownership has to be fixed before sshd starts"
        );
    }

    #[test]
    fn sshd_service_script_is_key_only_and_skips_without_binary() {
        let script = sshd_service_script();
        assert!(script.contains("PermitRootLogin=prohibit-password"));
        assert!(script.contains("PasswordAuthentication=no"));
        assert!(script.contains("no /usr/sbin/sshd"));
        assert!(script.contains("ssh-keygen -A"));
        assert!(script.contains("exec /usr/sbin/sshd -D"));
        assert!(script.contains("-f /etc/ssh/sshd_config.d/50-firecrab.conf"));
        assert!(script.starts_with("#!/etc/firecrab/busybox sh\n"));
    }

    #[cfg(unix)]
    #[test]
    fn authentication_records_go_to_a_private_file_instead_of_the_terminal() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            process::Command,
            time::{SystemTime, UNIX_EPOCH},
        };
        let root = std::env::temp_dir().join(format!(
            "firecrab-sshd-log-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let executable = |name: &str, body: &str| {
            let path = root.join(name);
            fs::write(&path, body).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path.to_string_lossy().into_owned()
        };
        // Ownership repair is exercised separately; this fixture must never chown host paths.
        let busybox = executable(
            "busybox",
            "#!/bin/sh\ncommand=$1; shift\n[ \"$command\" != chown ] || exit 0\nexec \"$command\" \"$@\"\n",
        );
        let keygen = executable("keygen", "#!/bin/sh\nexit 0\n");
        let daemon = executable(
            "sshd",
            "#!/bin/sh\n[ \"$(umask)\" = 0027 ] || exit 22\nwhile [ $# -gt 0 ]; do\n case \"$1\" in -e) exit 20;; -E) shift; log=$1;; esac\n shift\ndone\n[ -n \"${log-}\" ] || exit 21\nprintf 'Accepted publickey\\nFailed publickey\\nReceived disconnect\\n' >> \"$log\"\n",
        );
        let script = sshd_service_script()
            .replace("/etc/firecrab/busybox", &busybox)
            .replace("/usr/bin/ssh-keygen", &keygen)
            .replace("/usr/sbin/sshd", &daemon);
        let script = [
            "/root",
            "/run/sshd",
            "/var/lib/empty",
            "/var/empty",
            "/var/log/firecrab",
            "/dev/console",
        ]
        .into_iter()
        .fold(script, |s, path| {
            s.replace(
                path,
                root.join(path.trim_start_matches('/')).to_str().unwrap(),
            )
        });
        let service = root.join("service");
        fs::write(&service, script).unwrap();
        let result = Command::new("/bin/sh")
            .args(["-c", "umask 027; exec /bin/sh \"$1\"", "qa"])
            .arg(service)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty() && result.stderr.is_empty());
        let log = root.join("var/log/firecrab/sshd.log");
        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            "Accepted publickey\nFailed publickey\nReceived disconnect\n"
        );
        assert_eq!(
            fs::metadata(&log).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(log.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        fs::remove_dir_all(root).unwrap();
    }
}
