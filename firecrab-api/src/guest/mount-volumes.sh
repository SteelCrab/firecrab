#!/bin/sh
# UUIDs identify the filesystem even if virtio enumeration changes.
set -eu
BB=/etc/firecrab/busybox
[ -x "$BB" ] || BB=""
while read -r uuid target; do
    [ -n "$uuid" ] || continue
    # A parent filesystem can hide the directory prepared in the root image.
    rest=${target#/}
    directory=""
    while [ -n "$rest" ]; do
        component=${rest%%/*}
        directory="$directory/$component"
        if [ -L "$directory" ]; then
            echo "FIRECRAB_VOLUME_FAILED $uuid symlink-mount-path" >&2
            exit 1
        fi
        $BB mkdir -p "$directory"
        [ "$rest" != "$component" ] || break
        rest=${rest#*/}
    done
    device=""
    tries=0
    while [ "$tries" -lt 10 ]; do
        for candidate in /dev/vd*; do
            [ -b "$candidate" ] || continue
            identity=$($BB blkid "$candidate" 2>/dev/null) || continue
            case "$identity" in
                *" UUID=\"$uuid\""*) device=$candidate; break ;;
            esac
        done
        [ -z "$device" ] || break
        tries=$((tries + 1))
        $BB sleep 1
    done
    if [ -z "$device" ]; then
        echo "FIRECRAB_VOLUME_FAILED $uuid device-missing" >&2
        exit 1
    fi
    current=$($BB awk -v target="$target" '$2 == target { print $1; exit }' /proc/mounts)
    if [ -n "$current" ]; then
        identity=$($BB blkid "$current" 2>/dev/null) || exit 1
        case "$identity" in
            *" UUID=\"$uuid\""*) continue ;;
            *) echo "FIRECRAB_VOLUME_FAILED $uuid unexpected-mount" >&2; exit 1 ;;
        esac
    fi
    if ! $BB mount -t ext4 -o rw "$device" "$target"; then
        echo "FIRECRAB_VOLUME_FAILED $uuid mount-failed" >&2
        exit 1
    fi
done < /etc/firecrab/volumes
