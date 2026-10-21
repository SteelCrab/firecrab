set -u
device=$1
repair=null
changed=false
if [ -b "$device" ] && type=$(blkid -s TYPE -o value "$device") && [ "$type" = ext4 ] &&
   mounts=$(lsblk -nro MOUNTPOINTS "$device") &&
   ! printf '%s\n' "$mounts" | grep -q '[^[:space:]]'; then
    e2fsck -f -n "$device" >&2
    check=$?
    case $check in
        1|2|3|4|5|6|7|12)
            e2fsck -f -y "$device" >&2
            repair=$?
            changed=true
            if [ "$repair" -le 3 ]; then
                e2fsck -f -n "$device" >&2
                verify=$?
            else
                verify=$repair
            fi
            ;;
        *) verify=$check ;;
    esac
else
    check=8
    verify=8
fi
sync || verify=8
printf '{"check_exit":%s,"repair_exit":%s,"verify_exit":%s,"changed":%s}\n' "$check" "$repair" "$verify" "$changed"
