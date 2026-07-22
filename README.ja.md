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

## クイックスタート

### ターミナルセッション 1 — API サーバー

VM の状態管理と Firecracker プロセスの制御を担う Rust 製 REST API サーバー(`http://localhost:3000`)。

```sh
cargo run -p firecrab-api
```

### ターミナルセッション 2 — フロントエンド

VM の作成・確認・コンソール接続用 React ダッシュボードの開発サーバー(`http://localhost:8080`)。

```sh
cd firecrab-frontend
npm run dev
```

`http://localhost:8080/` を開く。使い方の詳細は [docs/web.md](docs/web.md) を参照。

## ライセンス

[Apache License, Version 2.0](./LICENSE) の下で配布される。
