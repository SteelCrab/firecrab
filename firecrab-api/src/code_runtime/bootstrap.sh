set -eu
umask 077
# Only trusted distro packages run as root. Repository content is never executed here.
if ! command -v python3 >/dev/null 2>&1; then
  if command -v apt-get >/dev/null 2>&1; then
    timeout 240 apt-get -o DPkg::Lock::Timeout=60 update >/dev/null 2>&1
    DEBIAN_FRONTEND=noninteractive timeout 240 apt-get -o DPkg::Lock::Timeout=60 install -y python3 >/dev/null 2>&1
  elif command -v apk >/dev/null 2>&1; then
    timeout 240 apk add --no-cache python3 >/dev/null 2>&1
  else
    exit 70
  fi
fi
