#!/bin/bash
set -Eeuo pipefail

fail() { echo "[FAIL] private Docker runtime: $*" >&2; exit 1; }
[[ -c /dev/kvm && -r /dev/kvm && -w /dev/kvm ]] \
  || fail 'the local Linux Docker engine must expose a usable /dev/kvm; enable nested KVM on Windows/WSL2'
[[ -c /dev/net/tun ]] || fail '/dev/net/tun is unavailable'
[[ $(cat /proc/self/cgroup) == '0::/' ]] \
  || fail 'a private cgroup v2 namespace is required'
[[ $(stat -fc %T /sys/fs/cgroup) == cgroup2fs ]] \
  || fail 'cgroup v2 is required'

# Docker's cgroup mount can be read-only. Mount only this container's namespace;
# do not bind the host hierarchy or share its cgroup namespace.
if ! test -w /sys/fs/cgroup/cgroup.procs; then
  umount /sys/fs/cgroup || fail 'cannot detach the private read-only cgroup mount'
  mount -t cgroup2 -o rw,nosuid,nodev,noexec cgroup2 /sys/fs/cgroup \
    || fail 'the engine must delegate a writable private cgroup v2 hierarchy'
fi
test -w /sys/fs/cgroup/cgroup.procs \
  || fail 'the private cgroup hierarchy is not writable'

# The helper changes bridge forwarding settings dynamically. Expose only the
# network sysctls belonging to this container's private network namespace.
if ! test -w /proc/sys/net/ipv4/ip_forward; then
  mount --bind /proc/sys/net /proc/sys/net \
    && mount -o remount,bind,rw /proc/sys/net \
    || fail 'the engine must permit writable sysctls in the private network namespace'
fi
test -w /proc/sys/net/ipv4/ip_forward \
  || fail 'private network forwarding settings are read-only'
exec /sbin/init --log-target=console
