#!/usr/bin/env bash
# Build the private ingress engine at release time. End users need no compiler,
# distro nginx, PCRE, OpenSSL or zlib packages.
set -euo pipefail

[ "$#" -eq 1 ] || { printf 'Usage: %s <output-directory>\n' "$0" >&2; exit 2; }
[ "$(uname -s)" = Linux ] || { printf 'build ingress on Linux with musl-tools\n' >&2; exit 1; }
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p "$1"
output=$(cd -- "$1" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cc=${CC:-musl-gcc}
libc_license=${INGRESS_LIBC_LICENSE:-/usr/share/doc/musl/copyright}
[ -f "$libc_license" ] || { printf 'missing musl license: %s\n' "$libc_license" >&2; exit 1; }

python3 - "$root/packaging/ingress-sources.json" >"$work/sources" <<'PY'
import json, sys
for name, source in json.load(open(sys.argv[1])).items():
    print(name, source['version'], source['url'], source['sha256'])
PY
while read -r name version url digest; do
    archive="$work/$name.tar.gz"
    cached="${INGRESS_SOURCE_DIR:-$work}/$name-$version.tar.gz"
    if [ -f "$cached" ]; then
        cp "$cached" "$archive"
    else
        curl --proto '=https' --tlsv1.2 --fail --location --retry 3 "$url" -o "$archive"
    fi
    printf '%s  %s\n' "$digest" "$archive" | sha256sum --check --status
    tar -xzf "$archive" -C "$work"
    case "$name" in
        nginx) nginx_version=$version ;;
        pcre2) pcre_version=$version ;;
    esac
done <"$work/sources"

cd "$work/nginx-$nginx_version"
./configure \
    --with-cc="$cc" \
    --with-cc-opt='-Os' \
    --with-ld-opt='-static' \
    --build=firecrab-ingress \
    --prefix=/usr/local/lib/firecrab/ingress \
    --sbin-path=/usr/local/lib/firecrab/firecrab-ingress \
    --conf-path=/etc/firecrab-pipe/ingress.conf \
    --pid-path=nginx.pid \
    --error-log-path=stderr \
    --http-log-path=/dev/null \
    --http-client-body-temp-path=body \
    --http-proxy-temp-path=proxy \
    --with-pcre="$work/pcre2-$pcre_version" \
    --with-http_v2_module \
    --without-http_gzip_module \
    --without-http_fastcgi_module \
    --without-http_uwsgi_module \
    --without-http_scgi_module
make -j "${INGRESS_BUILD_JOBS:-2}"
# A renamed dynamic nginx would still need host libraries. Reject it here.
if readelf -l objs/nginx | grep -q INTERP || readelf -d objs/nginx | grep -q NEEDED; then
    printf 'firecrab-ingress must be statically linked\n' >&2
    exit 1
fi
"$root/scripts/verify-release-binary.sh" objs/nginx "$(uname -m)" musl
objs/nginx -V
mkdir -p "$output/ingress"
install -m 0755 objs/nginx "$output/firecrab-ingress"
install -m 0644 LICENSE "$output/ingress/LICENSE.nginx"
install -m 0644 "$work/pcre2-$pcre_version/LICENCE.md" "$output/ingress/LICENSE.pcre2"
install -m 0644 "$libc_license" "$output/ingress/LICENSE.musl"
install -m 0644 "$root/packaging/ingress-sources.json" "$output/ingress/sources.json"
printf 'built %s/firecrab-ingress\n' "$output"
