# /run is fresh tmpfs on each boot. Reapply the image's directory rules after
# first-boot packages are configured, before starting any OCI services.
if command -v systemd-tmpfiles >/dev/null 2>&1; then
  if ! systemd-tmpfiles --create --boot --prefix=/run --prefix=/var/run; then
    echo "FIRECRAB_RUNTIME_FAILED tmpfiles" >&2
    exit 1
  fi
fi
