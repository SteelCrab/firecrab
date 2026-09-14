# Install a minimal set of essential operational tools on first boot.
# For slim OCI images, run `update` first, then install the minimum required packages.
# If installation fails, do not create the completion stamp; retry on the next boot.
# Distroless images have no package manager, so rely on the basic BusyBox applets.

# First boot only. Container images rarely ship ping/curl.
if [ ! -f /etc/firecrab/base-packages.ok ]; then
  ok=0
  if [ -x /usr/bin/apt-get ]; then
    # Bound network work, never the dpkg transaction: killing `apt install`
    # after 25s left Ubuntu guests half-configured and without sshd. Recover
    # disks interrupted by that older script before resolving dependencies.
    # This runs in the boot worker, so configuration cannot block the console.
    APT_OPTS="-o Acquire::http::Timeout=10 -o Acquire::https::Timeout=10 \
      -o Acquire::http::ConnectTimeout=5 -o Acquire::https::ConnectTimeout=5 \
      -o Acquire::Retries=1"
    PACKAGES="iputils-ping iproute2 ca-certificates curl procps openssh-server udev"
    # Missing dependencies can make configure fail; apt's fix-broken pass
    # below supplies them before it completes the pending configuration.
    env DEBIAN_FRONTEND=noninteractive /usr/bin/dpkg --configure -a || true
    if /usr/bin/timeout 25 env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get update -qq $APT_OPTS \
      && /usr/bin/timeout 120 env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get install -y -qq \
        --fix-broken --no-install-recommends --download-only $APT_OPTS $PACKAGES \
      && env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get install -y -qq \
        --fix-broken --no-install-recommends --no-download $PACKAGES; then
      ok=1
    fi
  elif [ -x /usr/bin/dnf ]; then
    /usr/bin/dnf install -y -q iputils iproute ca-certificates curl procps-ng openssh-server && ok=1
  elif [ -x /usr/bin/microdnf ]; then
    /usr/bin/microdnf -y install iputils iproute ca-certificates curl procps-ng openssh-server && ok=1
  elif [ -x /usr/bin/yum ]; then
    /usr/bin/yum install -y -q iputils iproute ca-certificates curl procps-ng openssh-server && ok=1
  elif [ -x /sbin/apk ]; then
    /sbin/apk add --no-cache iputils iproute2 ca-certificates curl procps openssh && ok=1
  elif [ -x /usr/bin/apk ]; then
    /usr/bin/apk add --no-cache iputils iproute2 ca-certificates curl procps openssh && ok=1
  elif [ -x /usr/bin/zypper ]; then
    /usr/bin/zypper --non-interactive install -y iputils iproute2 ca-certificates curl procps openssh udev && ok=1
  elif [ -x /usr/bin/pacman ]; then
    /usr/bin/pacman -Sy --noconfirm --needed iputils iproute2 ca-certificates curl procps-ng openssh && ok=1
  else
    ok=1
  fi
  if [ "$ok" -eq 1 ]; then
    $BB touch /etc/firecrab/base-packages.ok
  else
    echo "FIRECRAB_PACKAGES_FAILED base-packages (will retry next boot)" >/dev/console
  fi
fi