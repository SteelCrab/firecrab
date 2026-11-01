//! Opt-in QA against explicitly owned resources. Not an automated coverage input.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

#[test]
#[ignore = "Explicit owned qa-terminal VM; exercises real SSH logging without restarting the VM"]
fn real_disposable_vm_keeps_authentication_logs_out_of_the_console() {
    let home = PathBuf::from(std::env::var("FIRECRAB_TERMINAL_QA_HOME").unwrap());
    let id: Uuid = std::env::var("FIRECRAB_TERMINAL_QA_VM")
        .unwrap()
        .parse()
        .unwrap();
    let port = super::super::super::dev::instance::api_port(&home).unwrap();
    let base = format!("http://127.0.0.1:{port}/api/vms/{id}");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let vm: Value = client.get(&base).send().unwrap().json().unwrap();
    assert!(vm["name"].as_str().unwrap().starts_with("qa-terminal-"));
    let runtime = Runtime::for_vm(&home, id, false).unwrap();
    assert!(
        runtime.records().unwrap().is_empty(),
        "Do not touch a VM with agent sessions"
    );
    let guest = runtime.guest.as_ref().unwrap();
    let script = fs::read_to_string(std::env::var("FIRECRAB_TERMINAL_QA_SCRIPT").unwrap()).unwrap();
    assert!(script.contains("exec /usr/sbin/sshd -D -E \"$log_file\""));
    let encoded = STANDARD.encode(script);
    let mask = guest.shell("umask", Duration::from_secs(5)).unwrap();
    let boot = guest
        .shell(
            "cat /proc/sys/kernel/random/boot_id",
            Duration::from_secs(5),
        )
        .unwrap();
    let probe = guest.command("printf ready > /tmp/firecrab-ssh-qa-ready; sleep 4; printf ACTIVE_SSH_SESSION_SURVIVED").unwrap().stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    guest.shell(&format!(r#"set -eu
for attempt in 1 2 3 4 5; do [ ! -f /tmp/firecrab-ssh-qa-ready ] || break; sleep .1; done
test -f /tmp/firecrab-ssh-qa-ready
pid=$(cat /run/firecrab/sshd.pid)
case "$pid" in *[!0-9]*|'') exit 1;; esac
tr '\000' ' ' < "/proc/$pid/cmdline" | grep -q 'sshd.*-D'
printf %s {encoded} | base64 -d > /etc/firecrab/services.d/sshd
chmod 755 /etc/firecrab/services.d/sshd
kill -TERM "$pid"
nohup /etc/firecrab/busybox setsid /etc/firecrab/services.d/sshd </dev/null >/run/firecrab/sshd-startup.log 2>&1 &
printf '%s' "$!" > /run/firecrab/sshd.pid
"#),Duration::from_secs(10)).unwrap();
    let result = probe.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "Existing SSH session ended during listener update"
    );
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "ACTIVE_SSH_SESSION_SURVIVED"
    );
    guest
        .shell(
            "printf '\\nFIRECRAB_SSH_QA_QUIET\\n' > /dev/console",
            Duration::from_secs(10),
        )
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            guest
                .shell("printf SSH_STILL_WORKS", Duration::from_secs(10))
                .unwrap(),
            "SSH_STILL_WORKS"
        );
    }
    let audit = guest.shell("test \"$(stat -c %a /var/log/firecrab/sshd.log)\" = 600; grep -c 'Accepted publickey' /var/log/firecrab/sshd.log",Duration::from_secs(10)).unwrap();
    assert!(audit.trim().parse::<u32>().unwrap() >= 3);
    let log: Value = client
        .get(format!("{base}/log"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    let tail = log["consoleLog"]
        .as_str()
        .unwrap()
        .rsplit("FIRECRAB_SSH_QA_QUIET")
        .next()
        .unwrap();
    assert!(
        !tail.contains("Accepted publickey")
            && !tail.contains("Received disconnect")
            && !tail.contains("Disconnected from user")
    );
    assert_eq!(
        guest
            .shell(
                "cat /proc/sys/kernel/random/boot_id",
                Duration::from_secs(5)
            )
            .unwrap(),
        boot
    );
    assert_eq!(guest.shell("umask", Duration::from_secs(5)).unwrap(), mask);
    println!(
        "Real SSH authentication records are private, the serial console stays quiet, and an existing SSH session survives. VM boot identity unchanged."
    );
}
