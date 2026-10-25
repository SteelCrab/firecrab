FROM debian:13-slim

ENV container=docker
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    systemd systemd-sysv dbus ca-certificates curl iproute2 nftables conntrack \
    dnsmasq dnsmasq-utils e2fsprogs util-linux findutils tar zstd xz-utils \
    socat acl build-essential pkg-config cmake musl-tools python3 binutils git \
    && apt-get clean && rm -rf /var/lib/apt/lists/* \
    && (getent group kvm >/dev/null || groupadd --system kvm) \
    && systemctl mask systemd-remount-fs.service systemd-udevd.service \
       systemd-udevd-control.socket systemd-udevd-kernel.socket \
       getty.target console-getty.service dnsmasq.service

COPY install.sh /opt/firecrab-bootstrap/install.sh
COPY scripts/ /opt/firecrab-bootstrap/scripts/
COPY dev-container-entrypoint.sh dev-container-bootstrap.sh dev-container-publish.sh /opt/firecrab-bootstrap/
COPY guest-dev.sh /opt/firecrab-bootstrap/guest-dev.sh
RUN chmod 0755 /opt/firecrab-bootstrap/*.sh \
    && printf '[Unit]\nDescription=Private Firecrab API relay\nAfter=network.target\n[Service]\nExecStart=/opt/firecrab-bootstrap/dev-container-publish.sh\nRestart=on-failure\n[Install]\nWantedBy=multi-user.target\n' \
       >/etc/systemd/system/firecrab-dev-publish.service \
    && systemctl enable firecrab-dev-publish.service

STOPSIGNAL SIGRTMIN+3
ENTRYPOINT ["/opt/firecrab-bootstrap/dev-container-entrypoint.sh"]
