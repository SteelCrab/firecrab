use super::*;
use core::assert_matches;

use std::collections::BTreeMap;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt as _;

use tar::{Builder, EntryType, Header};
use tempfile::{TempDir, tempdir};

fn header(entry_type: EntryType, size: u64, mode: u32) -> Header {
    let mut header = Header::new_gnu();
    header.set_entry_type(entry_type);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(size);
    header
}

fn append_entry(
    builder: &mut Builder<Vec<u8>>,
    path: &str,
    entry_type: EntryType,
    target: Option<&str>,
    data: &[u8],
    mode: u32,
) {
    let mut header = header(entry_type, data.len() as u64, mode);
    if let Some(target) = target {
        header
            .set_link_name_literal(target.as_bytes())
            .expect("set fixture link target");
    }
    builder
        .append_data(&mut header, path, Cursor::new(data))
        .expect("append fixture tar entry");
}

fn finish(builder: Builder<Vec<u8>>) -> Vec<u8> {
    builder.into_inner().expect("finish fixture tar")
}

fn layer(directory: &TempDir, name: &str, bytes: &[u8]) -> DecompressedLayer {
    let compressed_bytes = format!("compressed provision fixture {name}").into_bytes();
    let compressed_digest = Sha256Digest::of_bytes(&compressed_bytes);
    let compressed_path = directory.path().join(format!("{name}.blob"));
    std::fs::write(&compressed_path, &compressed_bytes).expect("write compressed fixture");
    let path = directory.path().join(format!("{name}.tar"));
    std::fs::write(&path, bytes).expect("write tar fixture");
    DecompressedLayer {
        source: CachedBlob {
            descriptor: Descriptor {
                media_type: OCI_LAYER_MEDIA_TYPE.to_owned(),
                digest: compressed_digest,
                size: compressed_bytes.len() as u64,
            },
            path: compressed_path,
        },
        diff_id: Sha256Digest::of_bytes(bytes),
        path,
        size: bytes.len() as u64,
    }
}

/// Merges one fixture layer and returns the published tree.
async fn merged(directory: &TempDir, name: &str, tar: &[u8]) -> MergedRootfs {
    let layers = validate_decompressed_layers(vec![layer(directory, name, tar)])
        .await
        .expect("validate provision fixture");
    let destination = directory.path().join(format!("{name}-rootfs"));
    merge_validated_layers(&layers, &destination)
        .await
        .expect("merge provision fixture")
}

/// Builds a 64-bit little-endian ELF image with the given program headers.
///
/// Real busybox is 1 MiB of machine code; every rule this stage enforces lives
/// in the header, so a hand-built one exercises the verifier exactly.
fn elf(machine: u16, e_type: u16, program_headers: &[[u8; 56]], trailer: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0_u8; 64];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2; // ELFCLASS64
    bytes[5] = 1; // ELFDATA2LSB
    bytes[6] = 1; // EI_VERSION
    bytes[16..18].copy_from_slice(&e_type.to_le_bytes());
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes()); // e_phoff
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes()); // e_ehsize
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes()); // e_phentsize
    bytes[56..58].copy_from_slice(&(program_headers.len() as u16).to_le_bytes());
    for entry in program_headers {
        bytes.extend_from_slice(entry);
    }
    bytes.extend_from_slice(trailer);
    bytes
}

/// One program header entry.
fn program_header(p_type: u32, p_offset: u64, p_filesz: u64) -> [u8; 56] {
    let mut entry = [0_u8; 56];
    entry[..4].copy_from_slice(&p_type.to_le_bytes());
    entry[8..16].copy_from_slice(&p_offset.to_le_bytes());
    entry[32..40].copy_from_slice(&p_filesz.to_le_bytes());
    entry
}

/// A static program for this host, carrying the applet names the guest calls.
fn static_program() -> Vec<u8> {
    let mut applets = vec![0_u8];
    for applet in ["sh", "ip", "udhcpc", "awk", "mount", "sleep", "cut"] {
        applets.extend_from_slice(applet.as_bytes());
        applets.push(0);
    }
    let machine = match Architecture::HOST {
        Architecture::X86_64 => 62,
        Architecture::Aarch64 => 183,
    };
    elf(machine, 2, &[program_header(1, 0, 0)], &applets)
}

/// Writes a program to disk and verifies it the way the pull path would.
async fn toolbox(directory: &TempDir, name: &str, bytes: &[u8]) -> ToolboxProgram {
    let path = directory.path().join(name);
    std::fs::write(&path, bytes).expect("write toolbox fixture");
    busybox::inspect_toolbox(&path, Architecture::HOST)
        .await
        .expect("verify toolbox fixture")
}

/// Records every path in a tree with its type, mode, and contents.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read snapshot directory") {
            let entry = entry.expect("read snapshot entry");
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).expect("stat snapshot entry");
            let relative = path
                .strip_prefix(root)
                .expect("snapshot is rooted")
                .to_owned();
            let kind = if metadata.file_type().is_symlink() {
                format!(
                    "symlink:{}",
                    std::fs::read_link(&path)
                        .expect("read snapshot link")
                        .display()
                )
            } else if metadata.is_dir() {
                pending.push(path.clone());
                format!("dir:{:o}", metadata.permissions().mode() & 0o7777)
            } else {
                format!(
                    "file:{:o}:{}",
                    metadata.permissions().mode() & 0o7777,
                    Sha256Digest::of_bytes(&std::fs::read(&path).expect("read snapshot file"))
                )
            };
            entries.insert(relative, kind);
        }
    }
    entries
}

fn read_guest(tree: &Path, guest_path: &str) -> Vec<u8> {
    std::fs::read(tree.join(guest_path.trim_start_matches('/')))
        .unwrap_or_else(|error| panic!("read {guest_path}: {error}"))
}

fn guest_mode(tree: &Path, guest_path: &str) -> u32 {
    std::fs::metadata(tree.join(guest_path.trim_start_matches('/')))
        .unwrap_or_else(|error| panic!("stat {guest_path}: {error}"))
        .permissions()
        .mode()
        & 0o7777
}

/// A minimal container tree: an application and nothing an init needs.
fn application_layer() -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "app/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "app/server",
        EntryType::Regular,
        None,
        b"binary",
        0o755,
    );
    finish(builder)
}

#[tokio::test]
async fn injecting_a_guest_installs_an_init_the_stock_kernel_command_line_finds() {
    let directory = tempdir().expect("create fixture directory");
    let program = static_program();
    let toolbox = toolbox(&directory, "busybox", &program).await;
    let merged = merged(&directory, "plain", &application_layer()).await;
    let tree = merged.path().to_owned();

    let provisioned = provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("inject a guest runtime");

    assert_eq!(provisioned.path(), tree);
    assert_eq!(provisioned.toolbox_digest(), toolbox.digest());
    // The kernel execs /sbin/init when the command line carries no init=.
    assert_eq!(
        std::fs::read_link(tree.join("sbin/init")).expect("read /sbin/init"),
        Path::new(provision::GUEST_TOOLBOX)
    );
    assert_eq!(read_guest(&tree, provision::GUEST_TOOLBOX), program);
    assert_eq!(guest_mode(&tree, provision::GUEST_TOOLBOX), 0o755);
    assert_eq!(guest_mode(&tree, "/etc/firecrab/rc.boot"), 0o755);
    assert_eq!(guest_mode(&tree, "/etc/firecrab/rc.console"), 0o755);
    assert_eq!(guest_mode(&tree, "/etc/inittab"), 0o644);
    assert_eq!(
        read_guest(&tree, "/etc/motd"),
        crate::rootfs::FIRECRAB_MOTD.as_bytes()
    );

    let inittab = String::from_utf8(read_guest(&tree, "/etc/inittab")).expect("inittab is text");
    assert!(inittab.contains(&format!(
        "::sysinit:{} sh /etc/firecrab/rc.boot",
        provision::GUEST_TOOLBOX
    )));
    assert!(inittab.contains(&format!(
        "::respawn:-{} sh /etc/firecrab/rc.console",
        provision::GUEST_TOOLBOX
    )));

    // The host fails the VM start unless one of these reaches the console.
    let boot = String::from_utf8(read_guest(&tree, "/etc/firecrab/rc.boot")).expect("script");
    assert!(boot.contains("FIRECRAB_NETWORK_READY $ipv4"));
    assert!(boot.contains("FIRECRAB_NETWORK_FAILED no-ipv4-address"));
    assert!(boot.contains("FIRECRAB_NETWORK_FAILED dns-unreachable"));
    assert!(
        boot.contains("$BB hostname -F /etc/hostname"),
        "busybox init must apply specialize_guest's hostname: {boot}"
    );
    assert!(
        boot.contains("/proc/sys/kernel/hostname"),
        "hostname must also be written through proc: {boot}"
    );
    assert!(
        boot.contains("apt-get install -y -qq fastfetch"),
        "boot should try to install fastfetch after the network is ready: {boot}"
    );
    assert!(
        boot.contains("iputils-ping") && boot.contains("base-packages.ok"),
        "first boot should install a small package set: {boot}"
    );
    assert!(
        boot.contains("openssh-server")
            || boot.contains("apk add --no-cache") && boot.contains("openssh"),
        "first boot should install openssh: {boot}"
    );
    assert!(
        boot.contains("/run/firecrab/$name.pid"),
        "each services.d entry must get its own pid file: {boot}"
    );
    assert!(
        boot.contains("C.UTF-8") && boot.contains("stty iutf8"),
        "boot must enable UTF-8 on the serial tty: {boot}"
    );
    assert_eq!(
        std::fs::read_link(tree.join("bin/ping")).expect("busybox ping on PATH"),
        Path::new(provision::GUEST_TOOLBOX)
    );
    assert!(!tree.join("bin/sudo").exists());
    assert!(!tree.join("usr/local/bin/systemctl").exists());
    assert!(!tree.join("bin/systemctl").exists());

    let console =
        String::from_utf8(read_guest(&tree, "/etc/firecrab/rc.console")).expect("console");
    assert!(
        console.contains("$BB cat /etc/motd"),
        "console must print MOTD: {console}"
    );
    assert!(
        console.contains("/usr/bin/fastfetch"),
        "console must run fastfetch when present: {console}"
    );
    assert!(
        console.contains("exec $BB sh"),
        "console must drop into ash: {console}"
    );

    // One source of truth for the FIRECRAB_USAGE format the host parses.
    assert_eq!(
        read_guest(&tree, crate::guest_agent::BIN_PATH),
        crate::guest_agent::AGENT_SCRIPT.as_bytes()
    );

    for mount_point in ["proc", "sys", "dev", "run", "tmp"] {
        assert!(
            tree.join(mount_point).is_dir(),
            "{mount_point} is a directory"
        );
    }
    assert_eq!(guest_mode(&tree, "/tmp"), 0o1777);
    assert!(tree.join("etc/firecrab/services.d").is_dir());
    let sshd = read_guest(&tree, crate::guest_ssh::GUEST_SSHD_SERVICE);
    let sshd = String::from_utf8(sshd).expect("sshd service utf8");
    assert!(sshd.contains("PermitRootLogin=prohibit-password"), "{sshd}");
    assert_eq!(
        guest_mode(&tree, crate::guest_ssh::GUEST_SSHD_SERVICE) & 0o111,
        0o111
    );
    // The image's own files are left exactly as they were.
    assert_eq!(read_guest(&tree, "/app/server"), b"binary");
}

#[tokio::test]
async fn an_image_that_already_ships_ping_keeps_its_own_binary() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "usr/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "usr/bin/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    append_entry(
        &mut builder,
        "usr/bin/ping",
        EntryType::Regular,
        None,
        b"real-ping",
        0o755,
    );
    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "has-ping", &finish(builder)).await;
    let tree = merged.path().to_owned();

    provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("inject");

    assert_eq!(read_guest(&tree, "/usr/bin/ping"), b"real-ping");
    assert!(
        tree.join("usr/bin/ping").is_file(),
        "the image ping must stay a regular file"
    );
}

#[test]
fn applet_on_path_checks_usual_directories() {
    let present = |path: &str| path == "/usr/bin/ping";
    assert!(provision::applet_on_path(present, "ping"));
    assert!(!provision::applet_on_path(present, "wget"));
    assert_eq!(provision::applet_link_path(true, "ping"), "/usr/bin/ping");
    assert_eq!(provision::applet_link_path(false, "ping"), "/bin/ping");
}

/// Debian bookworm has a glibc loader but no fastfetch package. The host
/// program must land at `/usr/bin/fastfetch` so the console does not wait
/// for a guest `apt-get install` that will never succeed.
#[tokio::test]
async fn a_glibc_tree_receives_the_injected_fastfetch() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "app/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "app/server",
        EntryType::Regular,
        None,
        b"binary",
        0o755,
    );
    append_entry(
        &mut builder,
        "lib64/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    append_entry(
        &mut builder,
        "lib64/ld-linux-x86-64.so.2",
        EntryType::Regular,
        None,
        b"ldso",
        0o755,
    );
    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "glibc", &finish(builder)).await;
    let tree = merged.path().to_owned();
    let program_path = directory.path().join("fastfetch");
    let program_bytes = static_program();
    std::fs::write(&program_path, &program_bytes).expect("write fastfetch fixture");
    let program = fastfetch::inspect_fastfetch(&program_path, Architecture::HOST, None)
        .await
        .expect("fixture is a host ELF");

    provision::inject_guest_runtime(merged, &toolbox, Some(&program))
        .await
        .expect("inject");

    assert_eq!(
        read_guest(&tree, fastfetch::GUEST_PATH),
        program_bytes.as_slice()
    );
    assert_eq!(guest_mode(&tree, fastfetch::GUEST_PATH), 0o755);
}

#[tokio::test]
async fn a_musl_tree_does_not_receive_glibc_fastfetch() {
    let directory = tempdir().expect("create fixture directory");
    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "musl", &application_layer()).await;
    let tree = merged.path().to_owned();
    let program_path = directory.path().join("fastfetch");
    std::fs::write(&program_path, static_program()).expect("write fastfetch fixture");
    let program = fastfetch::inspect_fastfetch(&program_path, Architecture::HOST, None)
        .await
        .expect("fixture is a host ELF");

    provision::inject_guest_runtime(merged, &toolbox, Some(&program))
        .await
        .expect("inject");

    assert!(
        !tree.join("usr/bin/fastfetch").exists(),
        "a musl tree must not get a glibc binary that would fail at exec"
    );
}

#[tokio::test]
async fn an_image_with_agetty_and_bash_uses_the_serial_getty() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "usr/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "usr/sbin/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    append_entry(
        &mut builder,
        "usr/sbin/agetty",
        EntryType::Regular,
        None,
        b"agetty",
        0o755,
    );
    append_entry(&mut builder, "bin/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "bin/bash",
        EntryType::Regular,
        None,
        b"bash",
        0o755,
    );
    append_entry(
        &mut builder,
        "etc/passwd",
        EntryType::Regular,
        None,
        b"root:x:0:0:root:/root:/bin/sh\ndaemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\n",
        0o644,
    );
    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "getty", &finish(builder)).await;
    let tree = merged.path().to_owned();
    provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("inject");

    let inittab = String::from_utf8(read_guest(&tree, "/etc/inittab")).expect("inittab");
    assert!(
        inittab.contains(
            "ttyS0::respawn:/usr/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 ttyS0 linux"
        ),
        "{inittab}"
    );
    assert!(
        !inittab.contains("rc.console"),
        "agetty replaces the ash wrapper: {inittab}"
    );
    let passwd = String::from_utf8(read_guest(&tree, "/etc/passwd")).expect("passwd");
    assert!(
        passwd
            .lines()
            .any(|line| line.starts_with("root:") && line.ends_with(":/bin/bash")),
        "root shell must be bash: {passwd}"
    );
    assert!(
        passwd.contains("daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin"),
        "other passwd lines must stay: {passwd}"
    );
    let securetty = String::from_utf8(read_guest(&tree, "/etc/securetty")).expect("securetty");
    assert!(
        securetty.lines().any(|line| line.trim() == "ttyS0"),
        "{securetty}"
    );
}

#[tokio::test]
async fn the_injected_toolbox_is_named_busybox_so_the_multiplexer_runs() {
    let directory = tempdir().expect("create fixture directory");
    let program = static_program();
    let toolbox = toolbox(&directory, "busybox", &program).await;
    let merged = merged(&directory, "mux", &application_layer()).await;
    let tree = merged.path().to_owned();
    provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("inject a guest runtime");

    // busybox treats argv[0]'s basename as the applet. A program named
    // firecrab-busybox prints "applet not found" and never runs DHCP.
    let init = std::fs::read_link(tree.join("sbin/init")).expect("read /sbin/init");
    assert_eq!(
        init.file_name().and_then(|name| name.to_str()),
        Some("busybox")
    );
    let toolbox_guest = if init.is_absolute() {
        init.to_string_lossy().into_owned()
    } else {
        format!("/sbin/{}", init.display())
    };
    assert_eq!(read_guest(&tree, &toolbox_guest), program);

    let inittab = String::from_utf8(read_guest(&tree, "/etc/inittab")).expect("inittab");
    assert!(
        inittab.contains(&format!(
            "::sysinit:{toolbox_guest} sh /etc/firecrab/rc.boot"
        )),
        "inittab must invoke the multiplexer by path: {inittab}"
    );

    let boot = String::from_utf8(read_guest(&tree, "/etc/firecrab/rc.boot")).expect("boot");
    assert!(
        boot.starts_with(&format!("#!{toolbox_guest} sh\n")),
        "boot shebang must name the multiplexer: {boot}"
    );

    let dhcp = String::from_utf8(read_guest(&tree, "/etc/firecrab/dhcp.script")).expect("dhcp");
    assert!(
        dhcp.starts_with(&format!("#!{toolbox_guest} sh\n")),
        "dhcp shebang must name the multiplexer: {dhcp}"
    );
    assert!(
        dhcp.contains(r#"$BB ip addr add "$ip/${mask:-24}" dev "$interface""#),
        "dhcp script must keep busybox parameter expansion: {dhcp}"
    );
}

#[tokio::test]
async fn the_boot_script_brings_the_link_up_before_running_the_dhcp_client() {
    let directory = tempdir().expect("create fixture directory");
    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "order", &application_layer()).await;
    let tree = merged.path().to_owned();
    provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("inject a guest runtime");

    let boot = String::from_utf8(read_guest(&tree, "/etc/firecrab/rc.boot")).expect("script");
    let link_up = boot
        .find("ip link set eth0 up")
        .expect("link is brought up");
    let dhcp = boot.find("udhcpc -i eth0").expect("dhcp client runs");
    // A bare udhcpc on a down link sends zero packets and hangs forever.
    assert!(link_up < dhcp, "the link must come up before udhcpc");
}

#[test]
fn the_boot_script_exposes_dev_fd_for_process_substitution() {
    let boot = provision::boot_script();
    assert!(
        boot.contains("ln -sf /proc/self/fd /dev/fd"),
        "official image entrypoints open /dev/fd/N: {boot}"
    );
    let proc_mount = boot.find("mount -t proc").expect("proc is mounted");
    let fd = boot.find("/dev/fd").expect("fd links");
    let services = boot.find("services").expect("services start");
    assert!(
        proc_mount < fd && fd < services,
        "/proc must exist before /dev/fd, and services must start after"
    );
}

#[tokio::test]
async fn inittab_commands_avoid_the_metacharacters_busybox_init_reroutes() {
    let directory = tempdir().expect("create fixture directory");
    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "inittab", &application_layer()).await;
    let tree = merged.path().to_owned();
    provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("inject a guest runtime");

    let inittab = String::from_utf8(read_guest(&tree, "/etc/inittab")).expect("inittab is text");
    for line in inittab.lines().filter(|line| !line.starts_with('#')) {
        let command = line.splitn(4, ':').nth(3).unwrap_or_default();
        // busybox init runs a command containing any of these through
        // `/bin/sh -c`, which a distroless image does not ship.
        assert!(
            !command.contains([
                '~', '`', '!', '$', '^', '&', '*', '(', ')', '=', '|', '\\', '{', '}', '[', ']',
                ';', '"', '\'', '<', '>', '?'
            ]),
            "inittab command must not need a shell: {command:?}"
        );
    }
}

#[tokio::test]
async fn usr_merged_images_are_provisioned_through_their_symlinked_sbin() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "usr/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "usr/sbin/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    // Ubuntu, Debian 12+, and Fedora all ship this. Refusing it would reject
    // the majority of real images outright.
    append_entry(
        &mut builder,
        "sbin",
        EntryType::Symlink,
        Some("usr/sbin"),
        &[],
        0o777,
    );
    let tar = finish(builder);

    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "usrmerge", &tar).await;
    let tree = merged.path().to_owned();
    provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("a usr-merged image must be provisionable");

    assert!(
        std::fs::symlink_metadata(tree.join("sbin"))
            .expect("stat sbin")
            .file_type()
            .is_symlink(),
        "the image's own /sbin link is preserved"
    );
    assert!(tree.join("etc/firecrab/busybox").is_file());
    assert_eq!(
        std::fs::read_link(tree.join("usr/sbin/init")).expect("read init link"),
        Path::new(provision::GUEST_TOOLBOX)
    );
}

#[tokio::test]
async fn a_read_only_usr_sbin_can_still_have_its_init_replaced() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "usr/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "usr/sbin/",
        EntryType::Directory,
        None,
        &[],
        0o555,
    );
    append_entry(
        &mut builder,
        "usr/lib/systemd/systemd",
        EntryType::Regular,
        None,
        b"systemd",
        0o755,
    );
    append_entry(
        &mut builder,
        "usr/sbin/init",
        EntryType::Symlink,
        Some("../lib/systemd/systemd"),
        &[],
        0o777,
    );
    append_entry(
        &mut builder,
        "sbin",
        EntryType::Symlink,
        Some("usr/sbin"),
        &[],
        0o777,
    );
    let tar = finish(builder);

    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "ol9-sbin", &tar).await;
    let tree = merged.path().to_owned();
    assert_eq!(guest_mode(&tree, "/usr/sbin"), 0o555);

    provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("Oracle Linux /usr/sbin 0555 must not block init replacement");

    assert_eq!(
        std::fs::read_link(tree.join("usr/sbin/init")).expect("read init link"),
        Path::new(provision::GUEST_TOOLBOX)
    );
    assert!(
        tree.join("usr/lib/systemd/systemd").is_file(),
        "the image's systemd must not be followed and overwritten"
    );
    assert_eq!(
        guest_mode(&tree, "/usr/sbin"),
        0o555,
        "injection must put the image's /usr/sbin mode back"
    );
}

#[tokio::test]
async fn a_read_only_usr_sbin_without_init_can_still_receive_the_toolbox_link() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "usr/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "usr/sbin/",
        EntryType::Directory,
        None,
        &[],
        0o555,
    );
    append_entry(
        &mut builder,
        "sbin",
        EntryType::Symlink,
        Some("usr/sbin"),
        &[],
        0o777,
    );
    let tar = finish(builder);

    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "rl9-sbin", &tar).await;
    let tree = merged.path().to_owned();
    assert_eq!(guest_mode(&tree, "/usr/sbin"), 0o555);
    assert!(!tree.join("usr/sbin/init").exists());

    provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("Rocky /usr/sbin 0555 must not block creating /sbin/init");

    assert_eq!(
        std::fs::read_link(tree.join("usr/sbin/init")).expect("created init link"),
        Path::new(provision::GUEST_TOOLBOX)
    );
    assert_eq!(guest_mode(&tree, "/usr/sbin"), 0o555);
}

#[tokio::test]
async fn a_late_failure_restores_a_read_only_usr_sbin_and_its_init() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "usr/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "usr/sbin/",
        EntryType::Directory,
        None,
        &[],
        0o555,
    );
    append_entry(
        &mut builder,
        "usr/sbin/init",
        EntryType::Symlink,
        Some("../lib/systemd/systemd"),
        &[],
        0o777,
    );
    append_entry(
        &mut builder,
        "sbin",
        EntryType::Symlink,
        Some("usr/sbin"),
        &[],
        0o777,
    );
    append_entry(
        &mut builder,
        "usr/local",
        EntryType::Regular,
        None,
        b"not a directory",
        0o644,
    );
    let tar = finish(builder);

    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "ol9-late", &tar).await;
    let tree = merged.path().to_owned();
    let before = snapshot(&tree);

    let error = provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect_err("a blocked /usr/local must fail after unlocking /usr/sbin");
    assert_matches!(
        error,
        ResolveError::GuestPathUnusable {
            reason: GuestPathViolation::NonDirectoryAncestor { .. },
            ..
        }
    );
    assert_eq!(snapshot(&tree), before);
    assert_eq!(guest_mode(&tree, "/usr/sbin"), 0o555);
    assert_eq!(
        std::fs::read_link(tree.join("usr/sbin/init")).expect("read restored init"),
        Path::new("../lib/systemd/systemd")
    );
}

#[tokio::test]
async fn an_image_supplied_init_is_replaced_without_touching_its_target() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "lib/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "lib/systemd",
        EntryType::Regular,
        None,
        b"systemd",
        0o755,
    );
    append_entry(
        &mut builder,
        "sbin/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    append_entry(
        &mut builder,
        "sbin/init",
        EntryType::Symlink,
        Some("/lib/systemd"),
        &[],
        0o777,
    );
    let tar = finish(builder);

    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "systemd", &tar).await;
    let tree = merged.path().to_owned();
    provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("inject over an image-supplied init");

    assert_eq!(
        std::fs::read_link(tree.join("sbin/init")).expect("read init link"),
        Path::new(provision::GUEST_TOOLBOX)
    );
    // The final component is never followed, so the link was replaced rather
    // than written through onto systemd's own program.
    assert_eq!(read_guest(&tree, "/lib/systemd"), b"systemd");
}

#[tokio::test]
async fn a_symlinked_ancestor_pointing_outside_the_tree_is_clamped_to_it() {
    let directory = tempdir().expect("create fixture directory");
    let outside = directory.path().join("outside");
    std::fs::create_dir(&outside).expect("create outside directory");
    std::fs::write(outside.join("inittab"), b"untouched").expect("write outside sentinel");

    let mut builder = Builder::new(Vec::new());
    append_entry(
        &mut builder,
        "etc",
        EntryType::Symlink,
        Some(&outside.to_string_lossy()),
        &[],
        0o777,
    );
    let tar = finish(builder);

    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "escape", &tar).await;
    let tree = merged.path().to_owned();
    provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("an escaping ancestor is re-rooted, not refused");

    // The absolute target was re-rooted at the tree, so the host file outside
    // it never saw a write.
    assert_eq!(
        std::fs::read(outside.join("inittab")).unwrap(),
        b"untouched"
    );
    let inside = tree.join(outside.strip_prefix("/").expect("absolute outside path"));
    assert!(inside.join("inittab").is_file(), "the write landed in-tree");
}

#[tokio::test]
async fn a_non_directory_ancestor_refuses_the_injection_and_restores_the_tree() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(
        &mut builder,
        "etc",
        EntryType::Regular,
        None,
        b"not a directory",
        0o644,
    );
    let tar = finish(builder);

    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "blocked", &tar).await;
    let tree = merged.path().to_owned();
    let before = snapshot(&tree);

    let error = provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect_err("a file where /etc belongs must fail the injection");
    assert_matches!(
        error,
        ResolveError::GuestPathUnusable {
            reason: GuestPathViolation::NonDirectoryAncestor { .. },
            ..
        }
    );
    // A failed injection is not allowed to damage the caller's merged tree.
    assert_eq!(snapshot(&tree), before);
}

#[tokio::test]
async fn a_symlink_cycle_is_refused_before_any_guest_file_is_written() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(
        &mut builder,
        "etc",
        EntryType::Symlink,
        Some("etc2"),
        &[],
        0o777,
    );
    append_entry(
        &mut builder,
        "etc2",
        EntryType::Symlink,
        Some("etc"),
        &[],
        0o777,
    );
    let tar = finish(builder);

    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "cycle", &tar).await;
    let tree = merged.path().to_owned();
    let before = snapshot(&tree);

    let error = provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect_err("a symlink cycle must fail the injection");
    assert_matches!(error,
        ResolveError::GuestPathUnusable {
            reason: GuestPathViolation::SymlinkLoop { limit },
            ..
        } if limit == 40);
    assert_eq!(snapshot(&tree), before);
}

#[tokio::test]
async fn guest_injection_refusals_render_operator_readable_messages() {
    let rendered = [
        ResolveError::GuestPathUnusable {
            path: "/sbin/init".to_owned(),
            reason: GuestPathViolation::SymlinkLoop { limit: 40 },
        },
        ResolveError::GuestInjectionIo {
            operation: "create guest file",
            path: PathBuf::from("/trees/rootfs/etc/inittab"),
            source: io::Error::other("disk is full"),
        },
        ResolveError::GuestInjectionCancelled {
            path: PathBuf::from("/trees/rootfs"),
        },
    ]
    .map(|error| error.to_string());

    assert!(rendered[0].contains("40 symbolic links"));
    assert!(rendered[1].contains("disk is full"));
    assert!(rendered[2].contains("cancelled"));
}

#[tokio::test]
async fn a_late_failure_restores_an_image_supplied_init() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(
        &mut builder,
        "sbin/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    append_entry(
        &mut builder,
        "sbin/init",
        EntryType::Regular,
        None,
        b"the image's own init",
        0o755,
    );
    append_entry(&mut builder, "usr/", EntryType::Directory, None, &[], 0o755);
    // The metrics agent is installed last, so a blocked /usr/local fails the
    // injection only after the init has already been displaced.
    append_entry(
        &mut builder,
        "usr/local",
        EntryType::Regular,
        None,
        b"not a directory",
        0o644,
    );
    let tar = finish(builder);

    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "late", &tar).await;
    let tree = merged.path().to_owned();
    let before = snapshot(&tree);

    let error = provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect_err("a blocked /usr/local must fail the injection");
    assert_matches!(
        error,
        ResolveError::GuestPathUnusable {
            reason: GuestPathViolation::NonDirectoryAncestor { .. },
            ..
        }
    );
    // Everything is put back, including the init that had been moved aside.
    assert_eq!(snapshot(&tree), before);
    assert_eq!(read_guest(&tree, "/sbin/init"), b"the image's own init");
}

#[tokio::test]
async fn a_directory_occupying_a_guest_file_path_is_refused() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "etc/", EntryType::Directory, None, &[], 0o755);
    // Deleting an image's directory tree to make room is not this stage's call.
    append_entry(
        &mut builder,
        "etc/inittab/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    let tar = finish(builder);

    let toolbox = toolbox(&directory, "busybox", &static_program()).await;
    let merged = merged(&directory, "dirpath", &tar).await;
    let tree = merged.path().to_owned();
    let before = snapshot(&tree);

    let error = provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect_err("a directory where /etc/inittab belongs must fail");
    assert_matches!(
        error,
        ResolveError::GuestPathUnusable {
            reason: GuestPathViolation::NonDirectoryAncestor { .. },
            ..
        }
    );
    assert_eq!(snapshot(&tree), before);
}

#[tokio::test]
async fn a_provisioned_tree_records_the_program_it_will_boot() {
    let directory = tempdir().expect("create fixture directory");
    let program = static_program();
    let toolbox = toolbox(&directory, "busybox", &program).await;

    let merged = merged(&directory, "recorded", &application_layer()).await;
    let provisioned = provision::inject_with_toolbox(merged, &toolbox)
        .await
        .expect("inject a guest runtime");

    assert_eq!(
        provisioned.toolbox_digest(),
        &Sha256Digest::of_bytes(&program)
    );
}

/// `debugfs`-audited: apt-get and zypper images ship no `systemd-udevd`
/// (issue #225), while dnf-based Rocky already carries it and apk-based
/// Alpine isn't systemd at all. Pull `udev` in only where it's confirmed
/// missing and a real, separate package.
#[test]
fn base_package_install_pulls_in_udev_where_systemd_ships_without_it() {
    let script = provision::BASE_PACKAGE_INSTALL;

    // Each branch's command may wrap onto a continuation line (apt-get's
    // `\`), so join unindented and compare each `elif` block, not one line.
    let joined = script.replace("\\\n", " ");
    let block_with = |needle: &str| {
        joined
            .lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line contains {needle:?} in:\n{joined}"))
    };

    assert!(
        block_with("apt-get install").contains("udev"),
        "Debian/Ubuntu ship no systemd-udevd on a minimal OCI base — #225"
    );
    assert!(
        block_with("zypper --non-interactive install").contains("udev"),
        "openSUSE ships no systemd-udevd on a minimal OCI base — #225"
    );

    // Rocky/RHEL already carries systemd-udev; Alpine isn't systemd; Arch
    // folds udev into the `systemd` package itself — adding a separate
    // `udev` package there would 404 and break the whole install chain.
    for needle in ["dnf install", "microdnf -y install", "yum install"] {
        assert!(
            !block_with(needle).contains("udev"),
            "{needle} line must stay untouched — dnf-family already ships systemd-udev"
        );
    }
    assert!(
        !block_with("apk add").contains("udev"),
        "Alpine is not systemd"
    );
    assert!(
        !block_with("pacman -Sy").contains("udev"),
        "Arch folds udev into the systemd package; a separate `udev` package does not exist"
    );
}
