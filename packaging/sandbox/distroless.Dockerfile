# syntax=docker/dockerfile:1
# Both input images must be operator-selected digest references for the same architecture.
ARG APP_IMAGE
ARG SSH_TOOLS_IMAGE
ARG TOOLBOX_IMAGE=busybox@sha256:8d7b1636e974e0adfd8d945955fca609304f0a56c18799dfd032d6e661382d84
FROM ${APP_IMAGE} AS application
FROM ${TOOLBOX_IMAGE} AS toolbox
FROM ${SSH_TOOLS_IMAGE} AS prepare
RUN apk add --no-cache openssh-server openssh-client lddtree && command -v lddtree
COPY --from=application / /application/
COPY --from=toolbox /bin/busybox /overlay/etc/firecrab/busybox
COPY firecrab-guest-console /overlay/usr/local/sbin/firecrab-console-resize
RUN set -eu; \
    mkdir -p /overlay/usr/sbin /overlay/usr/bin /overlay/usr/lib/ssh /overlay/etc /overlay/var/empty /overlay/run/sshd /overlay/bin; \
    for name in sh env base64 cat chmod chown chgrp cp cut head id mkdir mkfifo mv rm sed sleep tar timeout touch wc awk; do \
      if [ ! -e "/application/bin/$name" ] && [ ! -e "/application/usr/bin/$name" ]; then ln -s /etc/firecrab/busybox "/overlay/bin/$name"; fi; \
    done; \
    for program in /usr/sbin/sshd /usr/bin/ssh-keygen /usr/lib/ssh/sshd-*; do \
      [ -f "$program" ] || continue; \
      lddtree -l "$program" >/tmp/firecrab-ssh-dependencies; \
      [ -s /tmp/firecrab-ssh-dependencies ]; \
      while IFS= read -r dependency; do \
        case "$dependency" in /*) ;; *) exit 1 ;; esac; \
        [ -f "$dependency" ]; \
        mkdir -p "/overlay/opt/firecrab/ssh-runtime$(dirname "$dependency")"; \
        cp -L "$dependency" "/overlay/opt/firecrab/ssh-runtime$dependency"; \
      done </tmp/firecrab-ssh-dependencies; \
      mkdir -p "/overlay$(dirname "$program")"; \
      printf '#!/etc/firecrab/busybox sh\nexec /opt/firecrab/ssh-runtime/lib/ld-musl-%s.so.1 --library-path /opt/firecrab/ssh-runtime/lib:/opt/firecrab/ssh-runtime/usr/lib /opt/firecrab/ssh-runtime%s "$@"\n' "$(uname -m)" "$program" >"/overlay$program"; \
      chmod 755 "/overlay$program"; \
    done; \
    rm /tmp/firecrab-ssh-dependencies; \
    ssh_runtime=/overlay/opt/firecrab/ssh-runtime; \
    "$ssh_runtime/lib/ld-musl-$(uname -m).so.1" --library-path "$ssh_runtime/lib:$ssh_runtime/usr/lib" "$ssh_runtime/usr/sbin/sshd" -V; \
    if [ -f /application/etc/passwd ]; then cp /application/etc/passwd /overlay/etc/passwd; else printf 'root:x:0:0:root:/root:/bin/sh\n' >/overlay/etc/passwd; fi; \
    if [ -f /application/etc/group ]; then cp /application/etc/group /overlay/etc/group; else printf 'root:x:0:\n' >/overlay/etc/group; fi; \
    if ! grep -q '^sshd:' /overlay/etc/passwd; then \
      ssh_uid=65530; \
      while awk -F: -v uid="$ssh_uid" '$3==uid {found=1} END {exit !found}' /overlay/etc/passwd; do ssh_uid=$((ssh_uid-1)); [ "$ssh_uid" -gt 64500 ]; done; \
      printf 'sshd:x:%s:%s:sshd:/var/empty:/bin/sh\n' "$ssh_uid" "$ssh_uid" >>/overlay/etc/passwd; \
      printf 'sshd:x:%s:\n' "$ssh_uid" >>/overlay/etc/group; \
    fi; \
    chmod 755 /overlay/etc/firecrab/busybox /overlay/usr/local/sbin/firecrab-console-resize; \
    chmod 700 /overlay/var/empty
FROM ${APP_IMAGE}
COPY --from=prepare /overlay/ /
