<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.94%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
</p>

<h1 align="center">firecrab</h1>

<p align="center"><a href="https://firecracker-microvm.github.io/">AWS Firecracker</a> を基盤としたプライベート microVM クラウド</p>

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.ko.md">한국어</a> ·
  <a href="./README.zh.md">中文</a>
</p>

## 概要

firecrab は、自前の Linux ホスト上に
[AWS Firecracker](https://firecracker-microvm.github.io/) microVM ベースのプライベートクラウド
を構築する軽量なコントロールプレーンである。Firecracker は AWS Lambda・Fargate を動かしているのと
同じ KVM ベースの VMM（Virtual Machine Monitor）で、一般的な VM よりはるかに高速に（数百 ms 単位で）
起動しながら、ハードウェア仮想化レベルの分離をそのまま提供する。firecrab はこの Firecracker を
AWS に依存せず直接ホストすることで、オンプレミスのサーバーでも同じ利点（高速起動・強力な分離・
低オーバーヘッド）を得られるようにする。

既存の KVM・VMware ベースの重量級レガシー VM を、より軽量で高速な microVM へ移行する経路を想定して
設計されている。Web ダッシュボードまたは REST API から VM を作成・起動・停止・削除し、各 VM の
シリアルコンソールにブラウザから直接接続して、起動ログからシェルまでリアルタイムに確認できる。

Rust で書かれた API サーバー（`firecrab-api`）が VM の状態を SQLite に保存し、Firecracker
プロセスを直接管理する。カーネル・rootfs テンプレートはハッシュで完全性を検証してから配信される。
ブリッジ・TAP・ファイアウォールなど root 権限が必要なホストネットワーク操作は、権限分離された
別の helper プロセス（`firecrab-net-helper`）が Unix ソケット経由でのみ処理し、API サーバー自体は
非特権プロセスとして動作する。

## 主な機能

- VM のライフサイクル全体を扱う REST API + React ダッシュボード
- 複数の起動テンプレート（Ubuntu、Alpine）
- WebSocket によるリアルタイムシリアルコンソール
- SQLite によるステート管理、権限分離された helper プロセスによるホストネットワーク分離

## インストール(推奨)

KVM が使えてネットワークにつながる Linux ホストであれば:

```sh
git clone https://github.com/SteelCrab/firecrab && cd firecrab
sudo ./install.sh
```

これだけです。インストーラーが足りないものを自分で見つけて導入します — パッケージは
ホストにある管理コマンド(apt/dnf/zypper/pacman/apk)経由で、さらに Firecracker、
Rust ツールチェーン、ゲストイメージまで。そのうえでサービスアカウントを作成し、
systemd デーモン 2 つを導入して `http://127.0.0.1:3000/` にダッシュボードを提供します。
再実行しても安全で、重複ではなく修復として動きます。

```sh
./install.sh --check            # 何が足りないかを先に確認(root 不要・変更なし)
sudo ./install.sh --uninstall   # デーモンを削除。--purge を付けなければ VM データは残る
```

KVM だけは代わりに導入できません — `/dev/kvm` が無い場合は BIOS で仮想化を有効に
してください(このホスト自体が VM ならネステッド仮想化)。詳細は
[docs/task-host-install-script.md](docs/task-host-install-script.md) を参照。

## ソースから実行(開発)

インストールせずターミナル 3 つで動かします — 特権ネットワークヘルパー、API、そして
ブラウザから API へ届かせるプロキシ付きの Vite 開発サーバーです。

```sh
# 1 — 特権ネットワークヘルパー(bridge, TAP, ファイアウォール, DHCP)
cargo build -p firecrab-net-helper
sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
     ./target/debug/firecrab-net-helper

# 2 — API サーバー(リポジトリのルートで実行 — パスは作業ディレクトリ基準)
cargo run -p firecrab-api

# 3 — ダッシュボード開発サーバー
cd firecrab-frontend && npm run dev
```

`http://localhost:8080/` を開く。使い方の詳細は [docs/web.md](docs/web.md) を参照。

3 番目のターミナルを使わず、ビルド済みダッシュボードを API から配信することもできます。

```sh
cd firecrab-frontend && npm run build && cd ..
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api
# http://localhost:3000/
```

## ライセンス

[Apache License, Version 2.0](./LICENSE) の下で配布される。
