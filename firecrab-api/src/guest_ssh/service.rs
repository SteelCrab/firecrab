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
mkdir -p /run/sshd /root/.ssh
if [ ! -x /usr/sbin/sshd ]; then
  echo "FIRECRAB_SSHD skipped: no /usr/sbin/sshd" >/dev/console
  exit 0
fi
# An unprivileged import left these owned by the host user; sshd refuses to
# start on a privsep directory it does not own, and refuses the key under a
# home directory it does not own.
for dir in /var/lib/empty /var/empty /run/sshd; do
  [ -d "$dir" ] || continue
  chown 0:0 "$dir" 2>/dev/null
  chmod go-w "$dir" 2>/dev/null
done
chown 0:0 /root /root/.ssh /root/.ssh/authorized_keys 2>/dev/null
chmod 700 /root/.ssh
chmod 600 /root/.ssh/authorized_keys 2>/dev/null
ssh-keygen -A >/dev/null 2>&1
exec /usr/sbin/sshd -D -e \
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
}
