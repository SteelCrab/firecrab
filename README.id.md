<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.96%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <a href="./CHANGELOG.md"><img alt="Changelog" src="https://img.shields.io/badge/changelog-0.2.2-informational"></a>
</p>

```text
███████ ██ ██████  ███████  ██████ ██████   █████  ██████
██      ██ ██   ██ ██      ██      ██   ██ ██   ██ ██   ██
█████   ██ ██████  █████   ██      ██████  ███████ ██████
██      ██ ██   ██ ██      ██      ██   ██ ██   ██ ██   ██
██      ██ ██   ██ ███████  ██████ ██   ██ ██   ██ ██████
```

<p align="center">Platform microVM ringan untuk server pribadi Anda.</p>

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.ko.md">한국어</a> ·
  <a href="./README.zh.md">中文</a> ·
  <a href="./README.ja.md">日本語</a>
</p>

**firecrab menjalankan microVM [Firecracker](https://firecracker-microvm.github.io/) di satu
host Linux yang Anda kendalikan.** Membuat VM juga berarti memilih image, jaringan, lokasi
disk, serta kebijakan akses keluar (outbound) — langsung dari dashboard browser, CLI, atau REST API.

Dirancang untuk lingkungan microVM privat pada host tunggal: isolasi yang lebih kuat daripada
kontainer, tanpa memerlukan control plane cloud berskala penuh. firecrab bukan layanan hosting
dan bukan penjadwal multi-host (multi-host scheduler).

![Demo dashboard firecrab M2](assets/dashboard/firecrab-m2.gif)

## Instalasi

Anda memerlukan host Linux dengan `/dev/kvm`, akses jaringan, dan pengguna dengan izin `sudo`.
Jalankan penginstal sebagai pengguna biasa tersebut — **jangan** beri awalan `sudo`. Penginstal
akan mengunduh biner rilis dan hanya memanggil `sudo` untuk langkah paket, systemd, dan
konfigurasi host yang memang membutuhkannya.

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash
```

```sh
./install.sh --check              # laporkan prasyarat dan perubahan yang direncanakan
./install.sh --doctor             # diagnosis KVM, firewall, soket, dan konfigurasi host
./install.sh --libc musl          # pilih libc secara manual alih-alih deteksi otomatis gnu/musl
./install.sh --uninstall          # pertahankan data secara default
./install.sh --uninstall --purge  # hapus juga /var/lib/firecrab
```

Penginstal tidak dapat mengaktifkan KVM secara otomatis. Jika `/dev/kvm` tidak tersedia,
aktifkan virtualisasi perangkat keras (atau nested virtualization) terlebih dahulu. Seluruh
opsi, jalur instalasi, dan langkah pemecahan masalah tercantum dalam
[panduan instalasi](public-docs/installation.md).

### Memasang biner CLI jarak jauh

Klien mandiri `firecrab` mengelola host Linux jarak jauh dari Linux, macOS Apple Silicon,
atau Windows x86_64/ARM64. Klien ini tidak memasang Firecracker maupun layanan host lokal.

Pada Linux atau macOS, unduh dan verifikasi skrip penginstal rilis sebelum menjalankannya:

```sh
curl -fLO https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.sh
curl -fLO https://github.com/SteelCrab/firecrab/releases/latest/download/SHA256SUMS
grep ' install-cli.sh$' SHA256SUMS > install-cli.sh.sha256
if command -v sha256sum >/dev/null; then
  sha256sum -c install-cli.sh.sha256
else
  shasum -a 256 -c install-cli.sh.sha256
fi
sh install-cli.sh
```

Hapus klien mandiri dengan perintah `sh install-cli.sh --uninstall`. Tambahkan `--purge` untuk
turut menghapus profil host yang tersimpan di `~/.firecrab/config.toml`.

Linux secara otomatis memilih arsip GNU atau musl yang sesuai. Lokasi tujuan default adalah
`~/.local/bin/firecrab`.

Pada Windows PowerShell:

```powershell
Invoke-WebRequest https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.ps1 -OutFile install-cli.ps1
Invoke-WebRequest https://github.com/SteelCrab/firecrab/releases/latest/download/SHA256SUMS -OutFile SHA256SUMS
$expected = ((Get-Content SHA256SUMS | Where-Object { $_ -match ' install-cli\.ps1$' }) -split '\s+')[0]
if ((Get-FileHash install-cli.ps1 -Algorithm SHA256).Hash -ne $expected) { throw 'installer checksum mismatch' }
& ./install-cli.ps1
```

Penginstal Windows akan memilih versi x86_64 atau ARM64 dan menambahkan direktori instalasi
tingkat pengguna ke `PATH`. Lihat [profil host CLI](public-docs/firecrab-cli.md#host-profiles)
untuk menghubungkan klien yang terpasang ke host firecrab.

## Memulai Cepat

Buka `http://127.0.0.1:5523/` setelah instalasi selesai, kemudian:

1. Buat **MicroNetwork**.
2. Pilih image yang telah terpasang dan buat VM di jaringan tersebut.
3. Jalankan VM, tunggu hingga statusnya `running`, lalu buka **Terminal**.

Membuat jaringan terlebih dahulu merupakan langkah yang disengaja: firecrab tidak memiliki
subnet default tersembunyi, sehingga setiap VM berada pada jaringan yang secara eksplisit
dipilih oleh operator.

## Fitur yang Anda Dapatkan

- **Siklus hidup microVM** — buat, periksa, ubah VM yang tidak aktif, jalankan, hentikan,
  hapus, serta akses setiap VM melalui konsol serial browser.
- **Jaringan terisolasi** — **MicroNetwork** eksplisit, dengan setiap VM memegang IPv4, MAC,
  dan hostname persisten. Antarjaringan terisolasi satu sama lain, dengan akses internet
  atau isolasi egress per VM.
- **Image dan disk** — pasang template M2Image, impor image OCI dari container registry,
  bootstrap distribusi Linux yang didukung dalam builder VM sementara, dan tempatkan disk VM
  pada storage root yang dikonfigurasi atau pool **MicroStorage**.
- **Visibilitas** — pantau progres booting awal, log konsol, dan status host di dashboard,
  tersedia dalam bahasa Inggris dan Korea.
- **Permukaan hak akses minimal** — API berjalan tanpa hak istimewa (unprivileged); hanya
  layanan terpisah `firecrab-net-helper` yang memegang kapabilitas jaringan host yang diperlukan.

## Arsitektur

Satu host Linux. Satu API tanpa hak istimewa. Satu helper dengan kapabilitas terbatas.
Satu proses Firecracker per guest yang berjalan. Tanpa penjadwal multi-host.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/architecture/firecrab-architecture-dark.svg">
  <img alt="arsitektur firecrab dalam lima lapisan: klien eksternal dan registry image, lapisan kontrol unprivileged firecrab-api, lapisan jaringan firecrab-net-helper dengan batas kapabilitas, lapisan runtime Firecracker, dan lapisan MicroStorage" src="assets/architecture/firecrab-architecture-light.svg">
</picture>

| Lapisan | Komponen | Peran |
| --- | --- | --- |
| Eksternal | `firecrab-frontend` | Antarmuka web untuk VM, jaringan, image, penyimpanan, dan konsol |
| Eksternal | `firecrab-cli` | Operasi yang sama langsung dari terminal |
| Kontrol | `firecrab-api` | REST, WebSocket, siklus hidup, SQLite, verifikasi artefak |
| Jaringan | `firecrab-net-helper` | Bridge, TAP, DHCP, DNS, NAT, firewall, penerusan port (port forward) |
| Runtime | Firecracker | Satu proses per guest yang sedang aktif |
| Penyimpanan | MicroStorage | Kernel, rootfs image, disk VM, status SQLite |

Sebuah MicroNetwork adalah satu subnet IPv4 pada bridge-nya sendiri. Guest dalam jaringan yang
sama dapat berkomunikasi melalui bridge tersebut; komunikasi antarjaringan berbeda diblokir.
NAT internet memerlukan pengaktifan switch `internetEnabled` pada jaringan dan aturan
`egressPolicy` pada VM.

Image OCI yang diimpor bukanlah sistem operasi yang dapat di-boot secara langsung. firecrab
mengubah hierarki registry menjadi rootfs Firecracker, menjalankan busybox sebagai PID 1,
dan mengeksekusi entrypoint image sebagai layanan — sehingga `/proc/1/exe` mengarah ke
`/etc/firecrab/busybox`, bukan `init` bawaan image.

Rincian lengkap: [arsitektur](public-docs/architecture.md) ·
[MicroNetwork](public-docs/micro-network.md) · [image OCI](public-docs/oci.md) ·
[API](public-docs/api.md).

## Perbandingan

firecrab mengisi celah antara menjalankan `firecracker` secara manual dan mengelola OpenStack:
satu server, dashboard web, serta primitif terstruktur untuk image (**M2Image**), jaringan
(**MicroNetwork**), dan disk (**MicroStorage**). firecrab menukar fitur clustering dan HA demi
control plane sederhana yang dapat dipelajari dalam satu sore.

<details>
<summary>Tabel perbandingan lengkap</summary>

| Kategori | **Firecrab** | VMware / ESXi | KVM + libvirt | OpenStack | Firecracker mandiri |
| --- | --- | --- | --- | --- | --- |
| Unit dasar | **microVM** | VM | VM | VM | microVM |
| Virtualisasi | Firecracker + KVM | VMware hypervisor | KVM/QEMU | Sebagian besar KVM/QEMU | KVM |
| Tujuan utama | **Operasi microVM sederhana pada satu server** | Virtualisasi enterprise | Virtualisasi Linux serbaguna | Cloud privat skala besar | Menjalankan microVM |
| Kompleksitas pengelolaan | **Dirancang rendah** | Sedang | Sedang–tinggi | **Sangat tinggi** | Tinggi |
| Dashboard web | ✅ | ✅ | Konfigurasi terpisah | ✅ | ❌ |
| Image VM | **M2Image** | Template/Image | qcow2, dll. | Glance | Manual |
| Jaringan virtual | **MicroNetwork** | vSwitch | bridge/jaringan libvirt | Neutron | Implementasi manual |
| Manajemen disk | **MicroStorage** | Datastore/VMDK | qcow2/LVM, dll. | Cinder | Implementasi manual |
| Konsol browser | ✅ | ✅ | Perlu konfigurasi | ✅ | ❌ |
| Isolasi VM | **Kuat** | Kuat | Kuat | Kuat | **Kuat** |
| Kecepatan boot | **Sangat cepat** | Relatif lambat | Relatif lambat | Relatif lambat | **Sangat cepat** |
| Beban sumber daya | **Rendah** | Tinggi | Sedang | Tinggi | **Sangat rendah** |
| Control plane | **Minimal** | Sudah termasuk | Hampir tidak ada | **Skala besar** | Tidak ada |
| Operasi satu server | **Tujuan utama** | Didukung | Didukung | Tidak efisien | Didukung |
| Klaster / HA | Terbatas / rencana masa depan | ✅ | Konfigurasi terpisah | ✅ | ❌ |
| Integrasi Kubernetes | Kemungkinan runtime masa depan | Didukung | Didukung | Didukung | Tersedia integrasi containerd |
| Paling sesuai untuk | **Server pribadi, homelab, edge, server pengembangan** | Datacenter enterprise | Server Linux | Cloud skala besar | Infrastruktur serverless/kontainer |

</details>

## Dashboard

Navigasi bilah kiri membagi alur kerja harian ke dalam **MicroVM**, **Terminal** per VM,
**Networks**, dan **Images**.

### MicroVM

Buat VM berdasarkan nama, image, CPU, RAM, disk, lokasi penyimpanan, MicroNetwork, dan kebijakan
egress. Daftar VM memperbarui status, image, sumber daya, dan ID setiap tiga detik; VM yang sedang
berjalan menyediakan akses **Terminal** dan opsi **stop**. Pilih nama VM untuk melihat detail progres
booting, log, jaringan, dan penyimpanan.

![Pembuatan dan daftar MicroVM](assets/dashboard/microvm.png)

### Terminal

**Terminal** membuka konsol serial VM yang sedang berjalan di tab terpisah, menampilkan streaming
output boot dan prompt login secara real-time. Toolbar menyediakan penyesuaian tampilan, opsi salin
atau simpan log konsol, serta mode tampilan khusus terminal.

![Terminal serial browser VM](assets/dashboard/terminal.png)

### Networks

Buat **MicroNetwork** dengan menentukan nama, subnet CIDR, dan kebijakan internet. Tombol **Block
internet** dan **Enable internet** mengubah akses keluar berbasis NAT untuk seluruh jaringan.
Memilih baris jaringan akan menampilkan penggunaan alamat subnet, bridge/TAP, NAT, firewall, dan
daftar VM yang terhubung.

![Pembuatan dan daftar MicroNetwork](assets/dashboard/networks.png)

### Images

Daftar **M2Image** menampilkan ukuran dan status setiap image, seperti `Package ready` atau
`Installed`. Menu `…` menyediakan aksi yang sesuai dengan status, seperti memasang paket, bootstrap,
atau menghapus image. Hanya image yang berstatus terpasang yang dapat digunakan untuk membuat VM.

Layar yang sama memungkinkan pemeriksaan referensi OCI (`nginx:1.27`) untuk arsitektur host saat ini
dan mengimpornya sebagai template terdaftar. Proses impor berjalan sebagai background job lengkap
dengan indikator progres, laporan error, dan alias yang dihasilkan.

![Daftar M2Image](assets/dashboard/images.png)

Lihat [panduan image](public-docs/images.md), [panduan image OCI](public-docs/oci.md),
dan [panduan API](public-docs/api.md).

## Pengembangan dari Sumber

Gunakan tiga terminal: network helper, API, dan dashboard Vite. Jalankan API dari root repository
karena jalur data lokal bersifat relatif terhadap direktori kerja.

```sh
# Terminal 1 — operasi jaringan dengan hak akses khusus
cargo build -p firecrab-net-helper
sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  ./target/debug/firecrab-net-helper

# Terminal 2 — API dan manajer Firecracker
pkill -x firecrab-api 2>/dev/null || true
cargo run -p firecrab-api

# Terminal 3 — dashboard di http://localhost:8080/
pkill -f '[f]irecrab-frontend/node_modules/.bin/vite' 2>/dev/null || true
npm install --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
```

Untuk pengujian lokal yang menyerupai lingkungan produksi, bangun dashboard dan biarkan API menyediakannya:

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api
# Akses melalui: http://127.0.0.1:5523/
```

## Pengujian

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked

# cakupan kode (coverage), opsional secara lokal — perintah yang sama dengan CI
cargo llvm-cov --workspace --locked --lcov --output-path lcov.info

# lint frontend, pemeriksaan tipe, dan build
npm install --prefix firecrab-frontend
npm run lint --prefix firecrab-frontend
npm run build --prefix firecrab-frontend

# E2E browser untuk inspeksi OCI → impor, terhadap fixture registry lokal
npm install --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e

# validasi link dokumentasi + format CHANGELOG
python3 scripts/check-doc-links.py
python3 scripts/check-changelog.py
```

`cargo clippy` berjalan dengan `-D warnings` — satu peringatan saja akan menggagalkan CI.
Daftar lengkap pemeriksaan sebelum membuka PR tercantum di
[CONTRIBUTING.md](./CONTRIBUTING.md#checks-before-you-open-a-pr). Lihat juga
[panduan dashboard web](public-docs/dashboard.md).

## Dokumentasi

Dokumentasi teknis lengkap berbahasa Inggris di folder [`public-docs/`](public-docs/README.md)
mencakup arsitektur, instalasi, operasional, kontrak API, dan pemecahan masalah.

## Kontribusi

<p align="center">
  <a href="./CONTRIBUTING.md">
    <img src="assets/icons/contributors.png" alt="Contributors" width="96" />
  </a>
</p>

Lihat [CONTRIBUTING.md](./CONTRIBUTING.md) untuk [catatan maintainer](./CONTRIBUTING.md#a-note-from-the-maintainer),
panduan setup pengembangan, daftar pemeriksaan, standar pull request, dan aturan dokumentasi.

## Lisensi

Dilisensikan di bawah [Apache License, Version 2.0](./LICENSE).
