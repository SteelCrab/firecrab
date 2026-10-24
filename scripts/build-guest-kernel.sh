#!/usr/bin/env bash
# Build a versioned console-capable kernel and its exact corresponding source.
set -euo pipefail
[ "$#" -eq 3 ] || { printf 'Usage: %s <x86_64|aarch64> <build-directory> <output-directory>\n' "$0" >&2; exit 2; }
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
architecture=$1
mkdir -p "$2" "$3"
build=$(cd -- "$2" && pwd)
output=$(cd -- "$3" && pwd)
manifest="$root/packaging/guest-kernel.json"
mapfile -t values < <(python3 - "$manifest" "$architecture" <<'PY'
import json,sys
data=json.load(open(sys.argv[1]))
arch=data['architectures'][sys.argv[2]]
for key in ['version','linuxVersion','sourceUrl','sourceSha256','sourceDateEpoch']: print(data[key])
for key in ['basePackageUrl','basePackageSha256','baseConfig','makeArch','target','output']: print(arch[key])
PY
)
[ "${#values[@]}" -eq 11 ] || { printf 'Unsupported architecture or invalid manifest\n' >&2; exit 1; }
version=${values[0]}
linux_version=${values[1]}
source_url=${values[2]}
source_sha=${values[3]}
export SOURCE_DATE_EPOCH=${values[4]}
source="$build/source/linux-$linux_version"
objects="$build/objects/$architecture"
archive="$build/downloads/linux-$linux_version.tar.xz"
base="$build/downloads/base-$architecture.tar.zst"
mkdir -p "$build/downloads" "$build/source" "$objects"

fetch_checked() {
  local url=$1 path=$2 digest=$3
  if [ -f "$path" ] && printf '%s  %s\n' "$digest" "$path" | sha256sum -c - >/dev/null 2>&1; then return; fi
  curl -fL --retry 2 --output "$path.partial" "$url"
  printf '%s  %s\n' "$digest" "$path.partial" | sha256sum -c -
  mv "$path.partial" "$path"
}

# The two architecture builds can share one verified source tree safely.
(
  flock 9
  fetch_checked "$source_url" "$archive" "$source_sha"
  if [ ! -f "$source/.firecrab-source-ready" ]; then
    tar -xJf "$archive" -C "$build/source"
    touch "$source/.firecrab-source-ready"
  fi
) 9>"$build/source.lock"
fetch_checked "${values[5]}" "$base" "${values[6]}"
tar --zstd -xOf "$base" "${values[7]}" > "$objects/base.config"
cp "$objects/base.config" "$objects/.config"
"$source/scripts/config" --file "$objects/.config" \
  --enable VSOCKETS --enable VIRTIO_VSOCKETS \
  --set-str LOCALVERSION '-firecrab-vsock1' --disable LOCALVERSION_AUTO

cross=${KERNEL_CROSS_COMPILE:-}
case "$(uname -m):$architecture" in
  x86_64:aarch64) cross=${cross:-aarch64-linux-gnu-} ;;
  aarch64:x86_64) cross=${cross:-x86_64-linux-gnu-} ;;
esac
export KBUILD_BUILD_USER=firecrab KBUILD_BUILD_HOST=kernel-builder KBUILD_BUILD_VERSION=1
KBUILD_BUILD_TIMESTAMP=$(date -u -d "@$SOURCE_DATE_EPOCH" '+%a %b %e %T UTC %Y')
export KBUILD_BUILD_TIMESTAMP
make_args=(-C "$source" O="$objects" ARCH="${values[8]}" CROSS_COMPILE="$cross")
make "${make_args[@]}" olddefconfig
for option in VSOCKETS VIRTIO_VSOCKETS; do
  grep -qx "CONFIG_$option=y" "$objects/.config" || { printf 'Missing built-in %s\n' "$option" >&2; exit 1; }
done
make "${make_args[@]}" -j"${KERNEL_BUILD_JOBS:-4}" "${values[9]}"

image="vmlinux-$version-$architecture"
[ "$architecture" != aarch64 ] || image="Image-$version-$architecture"
stage="$objects/package"
sources="$objects/sources"
mkdir -p "$stage/kernel" "$stage/config" "$sources/scripts" "$sources/packaging" "$sources/config"
install -m 0644 "$objects/${values[10]}" "$stage/kernel/$image"
install -m 0644 "$objects/.config" "$stage/config/$image.config"
cp "$archive" "$sources/"
cp "$objects/base.config" "$sources/config/base.config"
cp "$objects/.config" "$sources/config/$image.config"
cp "$0" "$sources/scripts/build-guest-kernel.sh"
cp "$manifest" "$sources/packaging/guest-kernel.json"
cp "$root/LICENSE" "$sources/LICENSE.firecrab"
cat > "$sources/README" <<EOF
Exact source for Firecrab $version ($architecture).
Unpack linux-$linux_version.tar.xz. Copy config/$image.config to a separate
output directory's .config. Build with make ARCH=${values[8]} O=<output>
CROSS_COMPILE=$cross olddefconfig, then make -j4 ARCH=${values[8]} O=<output>
CROSS_COMPILE=$cross ${values[9]}. The output is ${values[10]}.
Use KBUILD_BUILD_USER=firecrab KBUILD_BUILD_HOST=kernel-builder
KBUILD_BUILD_VERSION=1 and KBUILD_BUILD_TIMESTAMP='$KBUILD_BUILD_TIMESTAMP'.
The upstream archive contains Linux's COPYING and LICENSES directory.
EOF
"${cross}gcc" --version > "$sources/toolchain.txt"
(cd "$sources" && sha256sum "linux-$linux_version.tar.xz" config/* scripts/* packaging/* > SHA256SUMS)
package="$output/vmlinux-$version.tar.zst"
source_package="$output/vmlinux-$version.sources.tar.zst"
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C "$sources" -cf - . | zstd -T1 -10 -f -o "$source_package"
source_package_sha=$(sha256sum "$source_package" | cut -d ' ' -f 1)
cp "$source/COPYING" "$stage/COPYING"
cp -R "$source/LICENSES" "$stage/"
cat > "$stage/SOURCE.txt" <<EOF
Corresponding source: https://registry.firecrab.dev/kernel/$version/$architecture/vmlinux-$version.sources.tar.zst
SHA256: $source_package_sha
The source archive contains the exact upstream source, build config and build instructions.
See COPYING and LICENSES for Linux licensing information.
EOF
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C "$stage" -cf - kernel config COPYING LICENSES SOURCE.txt | zstd -T1 -10 -f -o "$package"
sha256sum "$package" "$source_package" "$stage/kernel/$image"
printf 'Built %s for %s\n' "$version" "$architecture"
