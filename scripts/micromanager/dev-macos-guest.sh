#!/bin/bash
# Shared by macOS (SSH) and Windows (WSL stdin) service dev and its controls.
set -Eeuo pipefail
profile=${1:?build profile or service action is required}
root=/var/lib/firecrab/dev
bin_root=/usr/local/lib/firecrab-dev
unit_root=/etc/systemd/system
case "$profile" in
  debug|release|restore)
    channel=${2:?Rust toolchain is required}
    archive_name=${3:?source archive name is required}
    case "$archive_name" in incoming-*.tar) ;; *) echo 'invalid source archive name' >&2; exit 1 ;; esac
    [[ "$archive_name" != */* ]]
    [[ "$channel" =~ ^[a-zA-Z0-9.-]+$ ]]
    ;;
  start|stop|restart) ;;
  *) echo 'invalid build profile or service action' >&2; exit 1 ;;
esac

install -d -m 0700 "$root"
exec 9>"$root/deploy.lock"
if ! flock -n 9; then
  if [ -n "${archive_name:-}" ]; then rm -f "$root/$archive_name"; fi
  echo 'another service dev build/deployment is in progress' >&2
  exit 1
fi

helper_unit=firecrab-helper
if [ "$(systemctl show --property=LoadState --value firecrab-helper.service)" = not-found ]; then
  helper_unit=firecrab-net-helper
fi

case "$profile" in
  start|stop|restart)
    if [ "$profile" != start ]; then
      systemctl stop firecrab-api
      systemctl stop "$helper_unit"
    fi
    if [ "$profile" = stop ]; then
      echo '[PASS] guest: API + helper stopped'
      exit 0
    fi
    systemctl start "$helper_unit"
    for _ in $(seq 1 30); do
      [ -S /run/firecrab/net-helper.sock ] && break
      sleep 1
    done
    test -S /run/firecrab/net-helper.sock
    systemctl start firecrab-api
    for _ in $(seq 1 60); do
      if systemctl is-active --quiet "$helper_unit" && systemctl is-active --quiet firecrab-api \
        && curl -fs --max-time 2 http://127.0.0.1:5523/api/host >/dev/null; then
        echo "[PASS] guest: API + helper $profile complete"
        exit 0
      fi
      sleep 1
    done
    journalctl --no-pager -n 30 -u firecrab-api -u "$helper_unit" >&2
    echo 'guest services did not become ready' >&2
    exit 1
    ;;
esac

install -d -m 0755 "$bin_root"
backup=$(mktemp -d "$root/rollback.XXXXXX")
deploying=0
binaries=
previous_bin=$(readlink "$bin_root/bin" || true)
for unit in firecrab-api firecrab-helper firecrab-net-helper; do
  override="$unit_root/$unit.service.d/90-firecrab-dev.conf"
  if [ -f "$override" ]; then cp "$override" "$backup/$unit.conf"; fi
done

# Invoked by the EXIT trap, including a failed build or interrupted deployment.
# shellcheck disable=SC2317,SC2329
finish() {
  rc=$?
  trap - EXIT
  set +e
  if [ "$rc" -ne 0 ] && [ "$deploying" -eq 1 ]; then
    echo '[ROLLBACK] restoring previous guest executables' >&2
    systemctl stop firecrab-api "$helper_unit"
    if [ -n "$previous_bin" ]; then
      ln -s "$previous_bin" "$bin_root/bin.rollback"
      mv -Tf "$bin_root/bin.rollback" "$bin_root/bin"
    else
      rm -f "$bin_root/bin"
    fi
    for unit in firecrab-api firecrab-helper firecrab-net-helper; do
      override="$unit_root/$unit.service.d/90-firecrab-dev.conf"
      if [ -f "$backup/$unit.conf" ]; then
        cp "$backup/$unit.conf" "$override"
      else
        rm -f "$override"
      fi
    done
    systemctl daemon-reload
    systemctl start "$helper_unit"
    systemctl start firecrab-api
  fi
  rm -f "$root/$archive_name" "$bin_root/bin.next"
  rm -rf "$backup"
  if [ "$rc" -ne 0 ] && [ -n "$binaries" ]; then rm -rf "$binaries"; fi
  exit "$rc"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [ "$profile" != restore ]; then
  case "$(uname -m)" in
    aarch64) target=aarch64-unknown-linux-gnu ;;
    x86_64) target=x86_64-unknown-linux-gnu ;;
    *) echo 'Linux ARM64 or x86_64 guest required' >&2; exit 1 ;;
  esac
  echo '[BUILD] preparing Debian compiler and pinned Rust toolchain'
  if ! command -v cc >/dev/null || ! command -v pkg-config >/dev/null || ! command -v cmake >/dev/null; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y build-essential pkg-config cmake ca-certificates curl
  fi
  export CARGO_HOME="$root/cargo"
  export RUSTUP_HOME="$root/rustup"
  export PATH="$CARGO_HOME/bin:$PATH"
  if [ ! -x "$CARGO_HOME/bin/rustup" ]; then
    installer=$(mktemp)
    curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs -o "$installer"
    sh "$installer" -y --profile minimal --default-toolchain none --no-modify-path
    rm -f "$installer"
  fi
  # A pinned toolchain already in the cache needs no channel refresh. WSL DNS
  # can still be coming up after a cold start; do not let it break a cached build.
  if ! rustup run "$channel" rustc --version >/dev/null 2>&1; then
    rustup toolchain install "$channel" --profile minimal --no-self-update
  fi

  # Keep the compiler cache on the data disk, but replace the snapshot so deleted
  # local files cannot linger in the next build. Never copy host target artifacts.
  rm -rf "$root/source"
  install -d -m 0700 "$root/source"
  tar -xf "$root/$archive_name" -C "$root/source"
  # Windows Git checkouts may use CRLF; include_str! also embeds guest fragments.
  # Avoid GNU-only sed -i and \r syntax so the deployment contract runs with
  # macOS BSD tools too. Writing back preserves each staged file's permissions.
  find "$root/source/scripts" "$root/source/firecrab-api" -type f \
    \( -path "$root/source/scripts/*" -o -name '*.sh' \) -print0 > "$backup/script-paths"
  while IFS= read -r -d '' script; do
    sed $'s/\r$//' "$script" > "$backup/normalized-script"
    cat "$backup/normalized-script" > "$script"
  done < "$backup/script-paths"
  cd "$root/source"
  export CARGO_TARGET_DIR="$root/target"
  export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
  export CARGO_PROFILE_DEV_DEBUG=${CARGO_PROFILE_DEV_DEBUG:-1}
  flags=(--locked --target "$target" -p firecrab-api -p firecrab-helper)
  if [ "$profile" = release ]; then flags+=(--release); fi
  rustup run "$channel" cargo build "${flags[@]}"

  binaries=$(mktemp -d "$bin_root/build.XXXXXX")
  chmod 0755 "$binaries"
  for binary in firecrab-api firecrab-helper; do
    install -m 0755 "$CARGO_TARGET_DIR/$target/$profile/$binary" "$binaries/$binary"
  done
  # The API resolves these runtime tools next to its executable. The source
  # snapshot is root-only, so its compile-time fallback path is not accessible.
  for tool in extract-vmlinux extract-arm64-image; do
    install -m 0755 "scripts/firecracker-menual/$tool" "$binaries/$tool"
  done
fi

# The packaged binaries, service accounts, permissions and working directories
# remain installed. Only ExecStart changes, with a rollback if readiness fails.
deploying=1
systemctl stop firecrab-api "$helper_unit"
if [ "$profile" = restore ]; then
  for unit in firecrab-api firecrab-helper firecrab-net-helper; do
    rm -f "$unit_root/$unit.service.d/90-firecrab-dev.conf"
  done
else
  for unit in firecrab-helper firecrab-net-helper; do
    rm -f "$unit_root/$unit.service.d/90-firecrab-dev.conf"
  done
  ln -s "$binaries" "$bin_root/bin.next"
  mv -Tf "$bin_root/bin.next" "$bin_root/bin"
  for unit in firecrab-api "$helper_unit"; do
    install -d -m 0755 "$unit_root/$unit.service.d"
    binary=firecrab-api
    if [ "$unit" = "$helper_unit" ]; then binary=firecrab-helper; fi
    printf '[Service]\nExecStart=\nExecStart=%s/bin/%s\n' "$bin_root" "$binary" \
      >"$unit_root/$unit.service.d/90-firecrab-dev.conf"
  done
fi
systemctl daemon-reload
systemctl start "$helper_unit"
for _ in $(seq 1 30); do
  [ -S /run/firecrab/net-helper.sock ] && break
  sleep 1
done
test -S /run/firecrab/net-helper.sock
systemctl start firecrab-api
for _ in $(seq 1 60); do
  if systemctl is-active --quiet "$helper_unit" && systemctl is-active --quiet firecrab-api \
    && curl -fs --max-time 2 http://127.0.0.1:5523/api/host >/dev/null; then
    deploying=0
    if [ "$profile" = restore ]; then rm -f "$bin_root/bin"; fi
    case "$previous_bin" in
      "$bin_root"/build.*) rm -rf "$previous_bin" ;;
    esac
    echo "[PASS] guest: $profile API + helper are ready"
    exit 0
  fi
  sleep 1
done
journalctl --no-pager -n 30 -u firecrab-api -u "$helper_unit" >&2
echo 'guest services did not become ready; restoring previous executables' >&2
exit 1
