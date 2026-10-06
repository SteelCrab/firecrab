<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.96%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <a href="./CHANGELOG.md"><img alt="Changelog" src="https://img.shields.io/badge/changelog-0.3.1-informational"></a>
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
  <a href="./README.ja.md">日本語</a> ·
  <a href="./README.zh.md">中文</a> ·
  <a href="./README.id.md">Bahasa Indonesia</a>
</p>

![Demo Firecrab](assets/dashboard/firecrab-demo.gif)

## Ringkasan

<details>
<summary>Tujuan</summary>

Jalankan microVM Firecracker pada satu host Linux yang Anda kelola melalui dashboard browser, CLI, atau REST API. Cocok untuk server pribadi, homelab, dan lingkungan pengembangan.

</details>

<details>
<summary>Fitur utama</summary>

- **VM** — buat, mulai, hentikan, dan hapus microVM; akses konsol serial di browser.
- **Image dan disk** — template M2Image, impor image OCI, dan lokasi disk MicroStorage.
- **Jaringan** — subnet MicroNetwork dan kebijakan internet atau isolasi per VM.
- **Platform host** — langsung di Linux; melalui microManager di macOS dan Windows (Windows Preview).

</details>

<details>
<summary>Perbandingan platform</summary>

| Poin utama | **Firecrab** | [KVM + libvirt](https://libvirt.org/) | [OpenStack](https://docs.openstack.org/nova/latest/) |
| --- | --- | --- | --- |
| Tujuan | microVM pada satu host | VM serbaguna | Cloud privat |
| Virtualisasi | Firecracker + KVM | QEMU/KVM | Umumnya QEMU/KVM |
| Pengelolaan | Dashboard, CLI, REST | API libvirt, CLI; GUI terpisah | Horizon, CLI, REST |
| Deployment | Satu host | Pengelolaan per host | Layanan controller dan compute |

</details>

## Arsitektur

[Arsitektur terperinci](public-docs/architecture.md): lapisan microManager per OS, startup VM, pasokan image/kernel, fitur guest, dan pembaruan CLI.

### Sekilas Firecrab

![Sekilas Firecrab](assets/architecture/firecrab-at-a-glance.en.svg)

1. Gunakan browser atau CLI. Pilih M2Image, MicroNetwork, dan MicroStorage untuk membuat MicroVM.
2. Firecrab memverifikasi M2Image, menyiapkan MicroNetwork, dan membuat disk khusus VM di MicroStorage.
3. Satu proses Firecracker dijalankan per MicroVM, sehingga setiap VM melakukan boot dengan kernelnya sendiri.
4. Saat guest melaporkan jaringan siap, MicroVM berstatus running.

### Berjalan di berbagai platform

![Berjalan di berbagai platform](assets/architecture/firecrab-runs-anywhere.en.svg)

Di Linux, satu `install.sh` sudah cukup. Di macOS dan Windows, `firecrab service install` membuat VM Debian terkelola yang menjalankan Firecrab yang sama, dan dashboard yang sama dibuka di `localhost:5523`.

macOS memerlukan Apple silicon M3 atau lebih baru dan telah divalidasi pada Apple M5. Windows ditandai Preview karena microVM belum dapat dimulai di WSL2.

Logo Linux, Apple, dan Debian berasal dari simple-icons (CC0); ikon roda gigi dari Lucide (ISC). Semua logo dan merek dagang milik pemiliknya masing-masing.

## Instalasi

<details>
<summary>Instalasi di Linux, macOS, atau Windows</summary>

### Linux

Memerlukan Linux x86_64 atau ARM64, `/dev/kvm` yang dapat digunakan, jaringan, dan pengguna biasa dengan izin `sudo`. Jalankan penginstal sebagai pengguna tersebut, **tanpa awalan `sudo`**. Aktifkan virtualisasi perangkat keras atau nested virtualization terlebih dahulu jika KVM tidak tersedia.

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash
```

Opsi diagnosis dan penghapusan ada di [panduan instalasi](public-docs/installation.md). Untuk memasang CLI jarak jauh saja di Linux, gunakan prosedur `install-cli.sh` dengan verifikasi checksum di bawah; GNU/musl dipilih otomatis dan lokasi default adalah `~/.local/bin`.

### macOS

Memerlukan Apple silicon, macOS 15+, dan dukungan runtime untuk nested virtualization (M3 atau lebih baru; validasi lengkap pada M5/macOS 26.6.2). Verifikasi checksum penginstal, lalu pasang CLI dan helper:

```sh
curl -fLO https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.sh
curl -fLO https://github.com/SteelCrab/firecrab/releases/latest/download/SHA256SUMS
grep ' install-cli.sh$' SHA256SUMS > install-cli.sh.sha256
if command -v sha256sum >/dev/null; then
  sha256sum -c install-cli.sh.sha256
else
  shasum -a 256 -c install-cli.sh.sha256
fi && sh install-cli.sh
```

Selanjutnya periksa kemampuan host dan siapkan lingkungan Debian terkelola:

```sh
firecrab service doctor
firecrab service install
```

Penginstal CLI saja tidak memasang VM terkelola. microManager menggunakan `Virtualization.framework`, disk data persisten terpisah, dan layanan launchd yang menetap. Lihat [panduan macOS](public-docs/micromanager-macos.md).

### Windows

Memerlukan Windows x86_64 atau ARM64, WSL2 dari Microsoft Store, dan nested KVM. Unduh dan verifikasi penginstal CLI dalam sesi PowerShell biasa:

```powershell
Invoke-WebRequest https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.ps1 -OutFile install-cli.ps1
Invoke-WebRequest https://github.com/SteelCrab/firecrab/releases/latest/download/SHA256SUMS -OutFile SHA256SUMS
$expected = ((Get-Content SHA256SUMS | Where-Object { $_ -match ' install-cli\.ps1$' }) -split '\s+')[0]
if ((Get-FileHash install-cli.ps1 -Algorithm SHA256).Hash -ne $expected) { throw 'installer checksum mismatch' }
& ./install-cli.ps1
```

Buka sesi PowerShell baru jika `firecrab` belum ada di `PATH`, lalu periksa kemampuan dan jalankan instalasi. Instalasi ARM64 belum divalidasi dari awal hingga akhir. Lihat [panduan Windows](public-docs/micromanager-windows.md).

```powershell
firecrab service doctor
firecrab service install
```

**Batasan Windows:** guest sebelum v0.3.0 tidak dapat memulai MicroVM pada WSL2 standar, karena WSL2 tidak memiliki keluarga nftables `bridge` yang dibutuhkan net-helper-nya. Guest yang baru diprovisioning mendapatkan rilis terbaru yang menyertakan perbaikan tersebut, tetapi jalur ini belum divalidasi di Windows, dan `service install` tidak mengubah guest yang sudah diprovisioning. CLI Windows tetap dapat mengelola host Linux jarak jauh yang didukung.

</details>

## Menjalankan

<details>
<summary>Mulai dan hentikan di Linux, macOS, atau Windows</summary>

### Linux

Penginstal memulai kedua layanan systemd. Untuk memulai, memeriksa, atau menghentikannya kemudian:

```sh
sudo systemctl start firecrab-helper firecrab-api
systemctl status firecrab-helper firecrab-api
sudo systemctl stop firecrab-api firecrab-helper
```

### macOS

microManager memulai VM terkelola yang menetap dan tunnel API localhost:

```sh
firecrab service start
firecrab service status
firecrab service debug --logs --tail 100
firecrab service stop
```

### Windows

microManager menjaga distribusi WSL2 terkelola tetap berjalan melalui tugas terjadwal per pengguna:

```powershell
firecrab service start
firecrab service status
firecrab service debug --logs --tail 100
firecrab service stop
```

**Batasan Windows:** guest sebelum v0.3.0 tidak dapat memulai MicroVM pada WSL2 standar, karena WSL2 tidak memiliki keluarga nftables `bridge` yang dibutuhkan net-helper-nya. Guest yang baru diprovisioning mendapatkan rilis terbaru yang menyertakan perbaikan tersebut, tetapi jalur ini belum divalidasi di Windows, dan `service install` tidak mengubah guest yang sudah diprovisioning. CLI Windows tetap dapat mengelola host Linux jarak jauh yang didukung.

Saat sehat, buka `http://127.0.0.1:5523/`. Buat MicroNetwork, pilih image terpasang, buat dan mulai VM, lalu buka Terminal setelah `running`. Untuk host jarak jauh, atur [profil host CLI](public-docs/firecrab-cli.md#host-profiles).

</details>

## Menjalankan dari sumber

<details>
<summary>API dan dashboard Linux / CLI macOS dan Windows</summary>

Dari root repositori, gunakan [toolchain Rust](rust-toolchain.toml) yang dipatok, Node.js 22+, dan npm. Eksekusi VM juga memerlukan Linux KVM dan [prasyarat host](public-docs/installation.md).

### Linux

Gunakan tiga terminal. Helper berjalan dengan hak istimewa; API berjalan sebagai pengguna biasa. Path data lokal relatif terhadap root repositori.

```sh
# 1
cargo build -p firecrab-helper --locked
sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  ./target/debug/firecrab-helper

# 2
cargo run -p firecrab-api --locked

# 3
npm ci --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
# http://localhost:8080/
```

Untuk menyajikan dashboard hasil build melalui API, hentikan API pengembangan lalu jalankan:

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api --locked
# http://127.0.0.1:5523/
```

### macOS

Build CLI dari checkout dan helper native bertanda tangan, lalu pasang layanan terkelola. Gunakan `service start` jika sudah terpasang:

```sh
cargo build -p firecrab-cli --locked
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
./target/debug/firecrab service install
./target/debug/firecrab service status
```

### Windows

Build CLI dari checkout di PowerShell, lalu pasang layanan terkelola. Gunakan `service start` jika sudah terpasang:

```powershell
cargo build -p firecrab-cli --locked
.\target\debug\firecrab.exe service install
.\target\debug\firecrab.exe service status
```

Di macOS dan Windows, perintah ini menjalankan CLI/helper dari checkout dengan API guest yang terpasang. Perintah ini **tidak** membangun ulang sumber `firecrab-api` yang diedit di dalam Debian. Kembangkan runtime API/net-helper lengkap di Linux; pengembangan layanan terkelola dijelaskan di panduan platform di atas.

</details>

## Pengujian

<details>
<summary>Pemeriksaan, cakupan, dan E2E browser</summary>

Pemeriksaan umum dari root repositori:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
npm ci --prefix firecrab-frontend
npm run lint --prefix firecrab-frontend
npm run build --prefix firecrab-frontend
python3 scripts/check-doc-links.py
python3 scripts/check-changelog.py
```

Cakupan lokal opsional (memerlukan `cargo-llvm-cov`):

```sh
cargo llvm-cov --workspace --locked --lcov --output-path lcov.info
```

E2E browser tanpa boot guest:

```sh
npm ci --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
```

Di PowerShell, atur `$env:FIRECRAB_E2E_SKIP_GUEST_BOOT="1"` sebelum `npm test --prefix firecrab-e2e`. Boot guest dilewati, sehingga KVM maupun HTTP nginx tidak divalidasi. Eksekusi guest dan seluruh pemeriksaan penginstal ada di [TEST.md](public-docs/TEST.md), [daftar periksa Korea](public-docs/TEST.ko.md), dan [panduan E2E](firecrab-e2e/README.md).

</details>

## Penggunaan: nginx

<details>
<summary>Impor nginx → buat microVM → akses HTTP</summary>

Gunakan host Linux yang didukung atau microManager macOS dengan API dan CLI yang berjalan. Perintah berikut menggunakan shell POSIX di Linux/macOS. Dashboard menyediakan alur yang sama di Images → OCI Import, Networks, dan MicroVM.

1. Periksa dan impor image, lalu ulangi pemeriksaan status sampai pekerjaan berhasil dan `nginx-1.27` terpasang:

```sh
firecrab image inspect nginx:1.27
firecrab image import nginx:1.27
firecrab image import-status nginx-1.27
```

2. Buat jaringan dengan subnet yang tidak tumpang tindih. Ganti `NETWORK_ID` dengan UUID dari perintah pertama:

```sh
firecrab network create --name nginx-net --subnet-cidr 172.31.20.0/24
firecrab vm create --name nginx-demo --template nginx-1.27 --network NETWORK_ID
```

3. Ganti `VM_ID` dengan UUID hasil pembuatan VM, mulai VM, lalu tunggu hingga daftar menunjukkan `running`:

```sh
firecrab vm start VM_ID
firecrab vm list
```

4. Tambahkan port host TCP `8081` → port guest `80` melalui REST API, lalu periksa respons nginx. PUT ini mengganti seluruh daftar port forward VM; sertakan aturan lain yang ingin dipertahankan.

```sh
curl -fsS -X PUT http://127.0.0.1:5523/api/vms/VM_ID/port-forwards \
  -H 'Content-Type: application/json' \
  -d '{"portForwards":[{"hostPort":8081,"guestPort":80,"protocol":"tcp"}]}'
```

Di Linux, periksa dari komputer lain menggunakan `http://FIRECRAB_HOST_IP:8081/`; DNAT tidak menyediakan akses loopback dari host itu sendiri. Di macOS, relay TCP menyediakan `http://127.0.0.1:8081/`. Izinkan port 8081 melalui firewall host/router untuk akses jarak jauh.

```sh
curl -I http://FIRECRAB_HOST_IP:8081/
# macOS
curl -I http://127.0.0.1:8081/
```

OCI import membangun rootfs yang dapat di-boot dengan `/etc/firecrab/busybox` sebagai PID 1, lalu menjalankan entrypoint nginx sebagai layanan. `EXPOSE 80` tidak membuat port forward host. Lihat [image OCI](public-docs/oci.md) dan [jaringan](public-docs/networking.md).

Untuk menghentikan VM contoh:

```sh
firecrab vm stop VM_ID
```

<details>
<summary>Panduan layar dashboard</summary>

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

</details>

</details>

## Dokumentasi dan kontribusi

Bahasa dokumentasi default adalah Inggris; tautan di atas menyediakan README Korea, Jepang, Tionghoa, dan Indonesia. Dashboard mendukung Inggris dan Korea.

- [public-docs/](public-docs/README.md): Instalasi, API, operasi, dan pemecahan masalah
- [CONTRIBUTING.md](CONTRIBUTING.md): Catatan pengelola, pengembangan, dan pemeriksaan PR

Dilisensikan dengan [Apache License, Version 2.0](LICENSE).
