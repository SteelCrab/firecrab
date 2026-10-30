#!/bin/sh
# Fixed bootstrap protocol. All substitutions are UUIDs, a SHA256, and base64 tar bytes.
set -eu
umask 077
BB=/etc/firecrab/busybox
[ -x "$BB" ] || BB=""
exec 9>/run/firecrab-project-__VOLUME__.lock
flock -x -w 2 9 || exit 19
root=/workspace/shared
[ ! -L /workspace ] && [ ! -L "$root" ] || exit 20
device=$($BB awk '$2 == "/workspace/shared" && $3 == "ext4" { print $1; exit }' /proc/mounts)
[ -n "$device" ] || exit 21
identity=$($BB blkid "$device")
case "$identity" in *' UUID="__VOLUME__"'*) ;; *) exit 22 ;; esac
marker="$root/.firecrab-workspace"
[ ! -L "$marker" ] || exit 23
expected='__PROJECT__:__DATABASE__:__DIGEST__'
if [ -f "$marker/ready" ]; then
    [ ! -L "$marker/ready" ] && [ "$(cat "$marker/ready")" = "$expected" ] || exit 24
else
    [ '__INITIALIZE__' = 1 ] || exit 25
    if [ -d "$marker" ]; then
        [ -f "$marker/pending" ] && [ "$(cat "$marker/pending")" = "$expected" ] || exit 26
    else
        for item in "$root"/* "$root"/.[!.]* "$root"/..?*; do
            [ -e "$item" ] || [ -L "$item" ] || continue
            [ "$item" = "$root/lost+found" ] || exit 27
        done
        mkdir "$marker"
        printf '%s' "$expected" > "$marker/pending"
        $BB sync
    fi
    $BB base64 -d > "$marker/import.tar" <<'FIRECRAB_PROJECT_ARCHIVE'
__ARCHIVE__
FIRECRAB_PROJECT_ARCHIVE
    digest=$($BB sha256sum "$marker/import.tar")
    [ "${digest%% *}" = '__DIGEST__' ] || exit 28
    $BB tar -xf "$marker/import.tar" -C "$root"
    $BB sync
    printf '%s' "$expected" > "$marker/ready.new"
    $BB sync
    mv "$marker/ready.new" "$marker/ready"
    $BB sync
fi
printf 'firecrab_project_volume_v1\n__PROJECT__\n__VOLUME__\n'
