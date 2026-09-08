# Cross-platform CLI verification

## Contents

- [Automated checks](#automated-checks)
- [Keyword checklist](#keyword-checklist)
- [Manual verification](#manual-verification)

## Automated checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test -p firecrab-cli --locked
bash scripts/test-install-cli-release.sh
pwsh -File scripts/test-install-cli.ps1
python3 scripts/check-doc-links.py
```

Native release CI runs `cargo test` and `cargo build` for these targets:

- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

## Keyword checklist

- `~/firecrab/config.toml`
- `USERPROFILE`
- `FIRECRAB_CONFIG_DIR`
- `host add/list/use/show/remove`
- serialized profile mutation and unknown-key rejection
- `--api` and `--host` precedence
- Linux-only host administration commands
- cross-platform raw terminal restoration
- SHA-256 release verification
- replacement without following an existing installer symlink
- user-writable installation and `PATH`

## Manual verification

### Terminal session 1 — Linux host, root required

```sh
sudo systemctl status firecrab-api firecrab-net-helper
curl -fsS http://127.0.0.1:5523/api/host
```

Keep the API on loopback unless remote authentication and TLS are configured.

### Terminal session 2 — Linux, macOS, or Windows client, no root

Forward the host loopback API through SSH, then save and exercise the endpoint:

```sh
ssh -fN -L 15523:127.0.0.1:5523 user@firecrab-host
firecrab host add dev http://127.0.0.1:15523 --use
firecrab host show
firecrab vm list
firecrab host list
```

Confirm `~/firecrab/config.toml` exists and contains `current_host = "dev"`.
On Windows, confirm the file under `%USERPROFILE%\firecrab\config.toml`.
