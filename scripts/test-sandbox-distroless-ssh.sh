#!/bin/sh
# Disposable image preparation smoke. This does not validate a MicroVM.
set -eu
umask 077
[ "${FIRECRAB_DISTROLESS_DOCKER_SMOKE-}" = 1 ] && [ -f /.dockerenv ] && [ "$(id -u)" = 0 ] || {
    echo 'Run only in the explicitly owned disposable Docker fixture.' >&2
    exit 64
}
[ "$#" = 3 ] || { echo 'Usage: script ROOTFS_ARCHIVE EPHEMERAL_KEYS OUTPUT_DIRECTORY' >&2; exit 64; }
archive=$1
keys=$2
output=$3
mkdir -p "$output"
root=$(mktemp -d /tmp/firecrab-distroless-ssh.XXXXXX)
server=
cleanup() {
    if [ -n "$server" ]; then kill "$server" 2>/dev/null || :; wait "$server" 2>/dev/null || :; fi
    rm -rf "$root"
}
trap cleanup EXIT HUP INT TERM
tar xf "$archive" -C "$root"
chmod 755 "$root"
mkdir -p "$root/dev" "$root/run/sshd" "$root/root/.ssh" "$root/etc/ssh"
for device in 'null 1 3' 'zero 1 5' 'random 1 8' 'urandom 1 9'; do
    set -- $device
    [ -e "$root/dev/$1" ] || mknod -m 666 "$root/dev/$1" c "$2" "$3"
done
root_shell=$(awk -F: '$1=="root" {print $7}' "$root/etc/passwd")
[ "$root_shell" = /bin/sh ] || { echo 'The prepared image must supply the operator root shell.' >&2; exit 79; }
cp "$keys/client.pub" "$root/root/.ssh/authorized_keys"
cp "$keys/ssh_host_ed25519_key" "$root/etc/ssh/ssh_host_ed25519_key"
chmod 700 "$root/root" "$root/root/.ssh"
chmod 600 "$root/root/.ssh/authorized_keys" "$root/etc/ssh/ssh_host_ed25519_key"
cat >"$root/etc/ssh/qa.conf" <<'CONFIG'
ListenAddress 127.0.0.1
Port 2222
HostKey /etc/ssh/ssh_host_ed25519_key
PermitRootLogin prohibit-password
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
AuthorizedKeysFile /root/.ssh/authorized_keys
AllowAgentForwarding no
AllowTcpForwarding no
X11Forwarding no
LogLevel DEBUG3
CONFIG
chroot "$root" /usr/sbin/sshd -D -e -f /etc/ssh/qa.conf >"$output/server.stdout" 2>"$output/server.stderr" &
server=$!
ready=false
for _ in $(seq 1 100); do
    if grep -q 'Server listening on 127.0.0.1 port 2222' "$output/server.stderr"; then ready=true; break; fi
    kill -0 "$server" 2>/dev/null || break
    sleep 0.05
done
[ "$ready" = true ] || { echo 'Prepared sshd failed to listen; inspect server.stderr.' >&2; exit 80; }
set +e
printf 'image-ssh-smoke\n' | ssh -F /dev/null -T -p 2222 \
    -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none \
    -o StrictHostKeyChecking=yes -o GlobalKnownHostsFile=/dev/null \
    -o "UserKnownHostsFile=$keys/known_hosts" -o ForwardAgent=no \
    -o ClearAllForwardings=yes -o PasswordAuthentication=no \
    -o KbdInteractiveAuthentication=no -o PreferredAuthentications=publickey \
    -o ConnectTimeout=3 -o ServerAliveInterval=3 -o ServerAliveCountMax=2 \
    -i "$keys/client" root@127.0.0.1 \
    '/usr/local/sbin/firecrab-console-resize --reuid 64555 --regid 64555 --clear-groups --no-new-privs /bin/env -i QA_NONCE=image-ssh-smoke QA_LITERAL=literal /usr/local/bin/firecrab-qa-probe --report' \
    >"$output/client.stdout" 2>"$output/client.stderr"
code=$?
set -e
printf '%s\n' "$code" >"$output/client.exit"
[ "$code" = 7 ] || { echo 'Prepared key-only SSH command failed; inspect client/server evidence.' >&2; exit 81; }
grep -q '^uid=64555$' "$output/client.stdout"
grep -q '^groups=0$' "$output/client.stdout"
grep -q '^noNewPrivs=1$' "$output/client.stdout"
grep -q '^stdin=696d6167652d7373682d736d6f6b650a$' "$output/client.stdout"
grep -q '^QA_STDERR:image-ssh-smoke$' "$output/client.stderr"
printf '%s\n' '{"kind":"prepared_image_key_only_ssh_smoke","microvmValidated":false,"v1ChannelValidated":false,"sshExit":7}' >"$output/report.json"
cat "$output/report.json"
