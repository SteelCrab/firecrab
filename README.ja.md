<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.96%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <a href="./CHANGELOG.md"><img alt="Changelog" src="https://img.shields.io/badge/changelog-0.1.0-informational"></a>
</p>

```text
███████ ██ ██████  ███████  ██████ ██████   █████  ██████
██      ██ ██   ██ ██      ██      ██   ██ ██   ██ ██   ██
█████   ██ ██████  █████   ██      ██████  ███████ ██████
██      ██ ██   ██ ██      ██      ██   ██ ██   ██ ██   ██
██      ██ ██   ██ ███████  ██████ ██   ██ ██   ██ ██████
```

<p align="center">自分のサーバーですぐ使える軽量 microVM プラットフォーム</p>

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.ko.md">한국어</a> ·
  <a href="./README.zh.md">中文</a>
</p>

firecrab は、管理下の Linux ホストで隔離された
[Firecracker](https://firecracker-microvm.github.io/) microVM を実行・管理します。Rust API、
ブラウザダッシュボード、二つの小さなシステムサービスを組み合わせ、VM 作成時にイメージ、
ネットワーク、ディスクの配置先、外向き通信ポリシーまで選べます。

コンテナより強い隔離が必要で、完全なクラウドコントロールプレーンまでは不要な、プライベートな
単一ホスト microVM 環境のためのツールです。ホステッドサービスや複数ホストのスケジューラでは
ありません。

## 主な機能

- **microVM の実行:** VM の作成・参照・停止中 VM の編集・起動・停止・削除と、VM ごとの
  ブラウザベースのシリアルコンソールを提供します。
- **隔離ネットワークの選択:** 明示的な **MicroNetwork** を作成し、各 VM を一つに配置します。
  IPv4・MAC・hostname は保持され、ネットワーク同士は隔離されます。VM ごとにインターネット許可
  または隔離の egress を選べます。
- **イメージとディスクの管理:** M2Image テンプレートのインストール・削除、レジストリからの
  OCI イメージ import、暫定 builder VM での対応ディストリビューションのブートストラップ、
  設定済みストレージルートまたは登録済み **MicroStorage** プールへの VM ディスク配置を
  サポートします。
- **状態の確認:** 英語・韓国語対応のダッシュボードで起動進捗、コンソールログ、ホスト状態を確認できます。
- **ホスト権限を最小化:** API は非特権で動作し、独立した `firecrab-net-helper` はホストネットワークに
  必要な capability だけを持ちます。

## プラットフォーム比較

| 区分 | **Firecrab** | VMware / ESXi | KVM + libvirt | OpenStack | Firecracker 単体 |
| --- | --- | --- | --- | --- | --- |
| 基本単位 | **microVM** | 一般的な VM | 一般的な VM | 一般的な VM | microVM |
| 仮想化基盤 | Firecracker + KVM | VMware Hypervisor | KVM/QEMU | 主に KVM/QEMU | KVM |
| 主な目的 | **単一サーバーでの簡単な microVM 運用** | エンタープライズ仮想化 | 汎用 Linux 仮想化 | 大規模 Private Cloud | microVM 実行 |
| 管理難易度 | **低さを重視** | 中 | 中~高 | **非常に高い** | 高 |
| Web ダッシュボード | ✅ | ✅ | 別途構築 | ✅ | ❌ |
| VM イメージ管理 | **M2Image** | Template/Image | qcow2 など | Glance | 手動管理 |
| 仮想ネットワーク | **MicroNetwork** | vSwitch | bridge/libvirt network | Neutron | 手動実装 |
| ディスク管理 | **MicroStorage** | Datastore/VMDK | qcow2/LVM など | Cinder | 手動実装 |
| ブラウザコンソール | ✅ | ✅ | 設定が必要 | ✅ | ❌ |
| VM 隔離 | **強い** | 強い | 強い | 強い | **強い** |
| 起動速度 | **非常に速い** | 比較的遅い | 比較的遅い | 比較的遅い | **非常に速い** |
| リソースオーバーヘッド | **低い** | 高い | 中 | 高い | **非常に低い** |
| Control Plane | **最小化** | あり | ほぼなし | **大規模** | なし |
| 単一サーバー運用 | **主要目標** | 可能 | 可能 | 非効率 | 可能 |
| クラスター/HA | 限定的 / 拡張領域 | ✅ | 別途構成 | ✅ | ❌ |
| Kubernetes 連携 | 将来 Runtime として拡張可能 | 可能 | 可能 | 可能 | containerd 連携可能 |
| 適した環境 | **個人サーバー、ホームラボ、Edge、開発サーバー** | 企業データセンター | Linux サーバー | 大規模クラウド | serverless/container 基盤 |

## アーキテクチャ

```text
ブラウザダッシュボード / REST クライアント
              │ HTTP + WebSocket
              ▼
  firecrab-api（Rust、SQLite、Firecracker プロセス管理）
       │                         │
       │ Unix socket             └── Firecracker → microVM ごとに一つのプロセス
       ▼
firecrab-net-helper（特権、capability 制限）
       └── bridge · TAP · nftables · dnsmasq
```

API はテンプレートアーティファクトを検証してから使用し、インストール済みの環境ではビルド済み
ダッシュボードも直接配信します。詳細は[アーキテクチャ](public-docs/architecture.md)を参照してください。

## Linux ホストへのインストール

`/dev/kvm`、ネットワーク接続、`sudo` を実行できる一般ユーザーがいる Linux ホストが必要です。
インストーラはその一般ユーザーとして実行し、スクリプトの前に **`sudo` を付けないで**ください。
リリースのバイナリを取得し、パッケージ、systemd、ホスト設定など権限が必要な個別操作だけで
内部的に `sudo` を使います。

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash
```

よく使うインストーラのオプション:

```sh
./install.sh --check                 # 前提条件と予定変更を確認
./install.sh --doctor                # KVM、ファイアウォール、socket、ホスト設定を診断
./install.sh --bin-dir target/release
./install.sh --uninstall         # デフォルトではデータを保持
./install.sh --uninstall --purge # /var/lib/firecrab も削除
```

標準のインストールは musl ホストバンドルを取得し、Alpine ゲストイメージを作ります。スクリプトは
KVM を有効化できません。`/dev/kvm` がない場合は、先にハードウェア仮想化（またはネステッド仮想化）を
有効にしてください。すべてのオプション、配置先、アップグレード、トラブルシューティングは
[インストールガイド](public-docs/installation.md)にあります。

## クイックスタート

インストール後に `http://127.0.0.1:5523/` を開き、次の順に進めます。

1. **MicroNetwork** を作成します。
2. インストール済みイメージを選び、そのネットワークに VM を作成します。
3. VM を起動し、`running` になったら **Terminal** を開きます。

先にネットワークを作るのは意図した流れです。firecrab には隠れたデフォルトサブネットがないため、
すべての VM は運用者が選択したネットワークに配置されます。

## ダッシュボードの画面案内

![firecrab M2 ダッシュボードデモ](assets/dashboard/firecrab-m2.gif)

ダッシュボードは左側のナビゲーションで、日常的な操作を **MicroVM**、VM ごとの
**ターミナル**、**ネットワーク**、**イメージ** に分けています。

### MicroVM

フォームで名前、イメージ、CPU、RAM、ディスク、ストレージの配置先、MicroNetwork、外向き通信
ポリシーを選んで VM を作成します。下の一覧は状態、イメージ、リソース、ID を 3 秒ごとに更新し、
実行中の VM には **ターミナル** と **停止** 操作が表示されます。VM 名を選択すると、起動の進捗、
ログ、ネットワーク、ストレージなどの詳細を確認できます。

![MicroVM の作成と一覧](assets/dashboard/microvm.png)

### ターミナル

実行中の VM の **ターミナル** は、別タブで開くブラウザのシリアルコンソールです。起動出力と
ログインプロンプトをリアルタイムに表示し、コマンドを入力できます。ツールバーで表示設定を変え、
コンソールログのコピー・保存やターミナルのみの表示に切り替えられます。下部パネルには VM の
一般情報、仕様、ネットワーク、ストレージが表示されます。

![VM のブラウザシリアルターミナル](assets/dashboard/terminal.png)

### ネットワーク

名前、サブネット CIDR、インターネットポリシーを指定して **MicroNetwork** を作成します。一覧には
各ネットワークのゲートウェイ、インターネット状態、ID が表示されます。**インターネットを遮断/接続**
でネットワーク全体の NAT 経由の外向き通信を変更したり、削除したりできます。行を選択すると、
サブネットのアドレス使用量、bridge/TAP、NAT、ファイアウォール、所属 VM の詳細を確認できます。

![MicroNetwork の作成と一覧](assets/dashboard/networks.png)

### イメージ

**M2Image** の一覧には、イメージごとのサイズと `パッケージ準備完了`・`インストール済み` などの
状態が表示されます。行を選択すると、別名、バージョン、最小ディスク、rootfs サイズ、状態、その
イメージを使う VM を確認できます。`…` メニューには状態に応じて、パッケージのインストール、
ブートストラップ、削除が表示されます。VM 作成に使えるのはインストール済みのイメージだけです。

同じ画面で OCI 参照（`nginx:1.27`）がこのホストのアーキテクチャに合うかを検査し、
テンプレートとして import できます。import はバックグラウンドジョブで、進捗・エラー・
登録された別名をページに出します。

![M2Image の一覧](assets/dashboard/images.png)

リクエスト形式、ライフサイクルの意味、エラー envelope は[API ガイド](public-docs/api.md)を、
イメージパッケージとブラウザ主導のブートストラップは[イメージガイド](public-docs/images.md)を、
OCI の inspect と import は [OCI イメージガイド](public-docs/oci.md) を参照してください。

## ソースから開発

network helper、API、Vite ダッシュボード用に三つの端末を使います。ローカルデータのパスは作業
ディレクトリ基準なので、API は必ずリポジトリルートから実行してください。

```sh
# 端末 1 — 特権ネットワーク操作
cargo build -p firecrab-net-helper
sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  ./target/debug/firecrab-net-helper

# 端末 2 — API と Firecracker マネージャ
# 任意: 以前の firecrab-api バイナリのみ終了（無ければ無視）
pkill -x firecrab-api 2>/dev/null || true
cargo run -p firecrab-api

# 端末 3 — ダッシュボード: http://localhost:8080/
# 任意: この checkout の Vite のみ終了（無ければ無視）
pkill -f '[f]irecrab-frontend/node_modules/.bin/vite' 2>/dev/null || true
npm install --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
```

ローカルで本番に近い形で実行するには、ダッシュボードをビルドして API に直接配信させます。

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api
# http://127.0.0.1:5523/
```

Rust のテストスイートは次で実行します。

```sh
cargo test --workspace
```

OCI inspect → import のブラウザ E2E（ローカルレジストリ fixture、Docker Hub なし）:

```sh
npm install --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
```

期待値は 1 passed、1 skipped です。
skipped のテストは VM を作って起動します。フラグなしの `npm test --prefix firecrab-e2e` には
KVM、Firecracker、`./scripts/dev-net-helper.sh` が必要です。
詳細は [firecrab-e2e/README.md](firecrab-e2e/README.md) を見てください。

開発時の注意点とブラウザのワークフローは[Web ダッシュボードガイド](public-docs/dashboard.md)にあります。

## ドキュメント

英語の技術ドキュメント [`public-docs/`](public-docs/README.md) には、アーキテクチャ、インストール、運用、
API 契約、トラブルシューティングがまとめられています。

## 貢献

<p align="center">
  <a href="./CONTRIBUTING.md#a-note-from-the-maintainer">
    <img src="assets/icons/contributors.png" alt="Contributors" width="96" />
  </a>
</p>

[メンテナからのメモ](./CONTRIBUTING.md#a-note-from-the-maintainer)、開発環境、チェック項目、PR の進め方、ドキュメント規約は
[CONTRIBUTING.md](./CONTRIBUTING.md) を参照してください。

## ライセンス

[Apache License, Version 2.0](./LICENSE) で提供します。
