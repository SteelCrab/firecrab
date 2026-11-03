install_node() {
  if command -v apk >/dev/null 2>&1; then
    apk add --no-cache nodejs npm
    return 0
  fi
  case $(uname -m) in
    x86_64) arch=x64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) echo "Node.js has no build for this CPU: $(uname -m)" >&2; return 1 ;;
  esac
  base=https://nodejs.org/dist/latest-v24.x
  work=$(mktemp -d /tmp/firecrab-node.XXXXXX)
  curl --connect-timeout 15 --retry 2 -fsSL "$base/SHASUMS256.txt" -o "$work/SHASUMS256.txt"
  file=$(grep -o "node-v24\.[0-9.]*-linux-$arch\.tar\.gz" "$work/SHASUMS256.txt" | head -n 1)
  if [ -z "$file" ]; then
    rm -rf "$work"
    echo "The Node.js release list is unavailable." >&2
    return 1
  fi
  curl --connect-timeout 15 --retry 2 -fsSL "$base/$file" -o "$work/$file"
  (cd "$work" && grep " $file\$" SHASUMS256.txt | sha256sum -c -)
  dest=/usr/local/lib/firecrab-node
  rm -rf "$dest.new"
  mkdir -p "$dest.new"
  tar -xzf "$work/$file" -C "$dest.new" --strip-components=1 --no-same-owner
  rm -rf "$dest" "$work"
  mv "$dest.new" "$dest"
  for name in node npm npx; do
    ln -sf "$dest/bin/$name" "/usr/local/bin/$name"
  done
}
