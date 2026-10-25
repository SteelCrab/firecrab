//! Authenticated SSH transport using each VM's existing operator key and pinned host key.
//! Only a task-owned unprivileged guest user runs model commands. No host shell is exposed.
use super::{SavedTask, workspace};
use crate::{artifacts::VmArtifactPaths, guest_ssh::VmSshPaths, model::VmPurpose, state::AppState};
use base64::{Engine, engine::general_purpose::STANDARD};
use firecrab_api_types::VmState;
use std::{
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use uuid::Uuid;

/// Captured guest identity shared with durable task records.
pub type Binding = super::GuestBinding;

/// Bounded guest process result, with stdout and stderr kept separate.
pub struct Output {
    /// Bytes received on stdout.
    pub stdout: Vec<u8>,
    /// Bytes received on stderr.
    pub stderr: Vec<u8>,
    /// SSH or guest exit status.
    pub code: i32,
}

/// Refuse internal, service-owned, stopped or unaddressed VMs before using any SSH key.
pub fn binding(state: &AppState, id: Uuid) -> io::Result<Binding> {
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .cloned()
        .ok_or_else(|| io::Error::other("MicroVM does not exist"))?;
    if vm.state != VmState::Running
        || vm.purpose != VmPurpose::Instance
        || state
            .store
            .cluster_worker(id)
            .map_err(io::Error::other)?
            .is_some()
        || state
            .store
            .vm_has_service_owner(id)
            .map_err(io::Error::other)?
    {
        return Err(io::Error::other(
            "Select a running user MicroVM without a MicroService owner",
        ));
    }
    if state
        .store
        .active_lease(id)
        .map_err(io::Error::other)?
        .is_none()
    {
        return Err(io::Error::other("MicroVM has no network address"));
    }
    Ok(Binding {
        vm_id: id,
        runtime_id: vm
            .last_runtime_id
            .ok_or_else(|| io::Error::other("MicroVM runtime is not ready"))?,
    })
}

fn ssh(state: &AppState, binding: &Binding, attempt: &Path) -> io::Result<Command> {
    let current = self::binding(state, binding.vm_id)?;
    if current.runtime_id != binding.runtime_id {
        return Err(io::Error::other(
            "MicroVM restarted; the previous execution cannot be reused",
        ));
    }
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&binding.vm_id)
        .cloned()
        .ok_or_else(|| io::Error::other("MicroVM disappeared"))?;
    let lease = state
        .store
        .active_lease(vm.id)
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::other("MicroVM address disappeared"))?;
    let keys = VmSshPaths::from_artifacts(&VmArtifactPaths::for_vm(
        &state.vms_dir_for(&vm.storage_root),
        vm.id,
    ));
    let executable = std::fs::metadata("/usr/bin/ssh")?;
    let private = std::fs::symlink_metadata(&keys.operator_private)?;
    if !executable.is_file()
        || executable.permissions().mode() & 0o111 == 0
        || !private.is_file()
        || private.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::other(
            "Sandbox requires /usr/bin/ssh and a private owner-only VM operator key",
        ));
    }
    let key = std::fs::read_to_string(&keys.host_public)?;
    let fields: Vec<_> = key.split_whitespace().take(2).collect();
    if fields.len() != 2
        || fields[0] != "ssh-ed25519"
        || !fields[1]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
    {
        return Err(io::Error::other("Expected VM host key is invalid"));
    }
    let known = attempt.join("known_hosts");
    std::fs::write(
        &known,
        format!("{} {} {}\n", lease.ipv4, fields[0], fields[1]),
    )?;
    std::fs::set_permissions(&known, std::fs::Permissions::from_mode(0o600))?;
    let mut cmd = Command::new("/usr/bin/ssh");
    cmd.env_clear().env("PATH", "/usr/bin:/bin");
    cmd.args([
        "-F",
        "/dev/null",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "IdentityAgent=none",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "GlobalKnownHostsFile=/dev/null",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=2",
        "-o",
        "LogLevel=ERROR",
    ])
    .arg("-o")
    .arg(format!("UserKnownHostsFile={}", known.display()))
    .arg("-i")
    .arg(&keys.operator_private)
    .arg(format!("root@{}", lease.ipv4))
    .arg("/bin/sh -s");
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(cmd)
}

async fn read_limited(mut reader: impl AsyncRead + Unpin, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        if output.len() + n > limit {
            return Err(io::Error::other("Guest output limit exceeded"));
        }
        output.extend_from_slice(&chunk[..n]);
    }
    Ok(output)
}

async fn capture(
    mut command: Command,
    input: Vec<u8>,
    limit: usize,
    timeout: Duration,
) -> io::Result<Output> {
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("SSH stdin unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("SSH stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("SSH stderr unavailable"))?;
    let work = async {
        let write = async move {
            stdin.write_all(&input).await?;
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<_, io::Error>(())
        };
        let (_, stdout, stderr, status) = tokio::try_join!(
            write,
            read_limited(stdout, limit),
            read_limited(stderr, 65536),
            child.wait()
        )?;
        Ok::<_, io::Error>(Output {
            stdout,
            stderr,
            code: status.code().unwrap_or(255),
        })
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            Err(io::Error::other("Guest transport timed out"))
        }
    }
}

fn script(
    action: &str,
    id: Uuid,
    invocation: Option<Uuid>,
    payload: &str,
    seconds: u64,
) -> Vec<u8> {
    SCRIPT
        .replace("__ID__", &id.simple().to_string())
        .replace("__ACTION__", action)
        .replace(
            "__COMMAND__",
            &invocation.unwrap_or_default().simple().to_string(),
        )
        .replace("__PAYLOAD__", payload)
        .replace("__SECONDS__", &seconds.to_string())
        .into_bytes()
}
fn attempt(saved: &SavedTask) -> io::Result<PathBuf> {
    saved
        .workspace
        .clone()
        .ok_or_else(|| io::Error::other("Task workspace is unavailable"))
}

/// Upload a sanitized snapshot, run a bounded command, then export changes before cleanup.
pub async fn execute(
    state: &AppState,
    saved: &mut SavedTask,
    command: &str,
    seconds: u64,
) -> io::Result<Output> {
    super::access::attempt(state, saved).map_err(io::Error::other)?;
    if command.trim().is_empty()
        || command.len() > 8192
        || command.contains('\0')
        || !(1..=120).contains(&seconds)
    {
        return Err(io::Error::other("Invalid guest command or time limit"));
    }
    let binding = saved
        .guest
        .ok_or_else(|| io::Error::other("Select a MicroVM before running commands"))?;
    let attempt = attempt(saved)?;
    let probe = capture(
        ssh(state, &binding, &attempt)?,
        script("probe", saved.task.attempt_id, None, "", 0),
        1024,
        Duration::from_secs(10),
    )
    .await?;
    if probe.code != 0 {
        return Err(io::Error::other(
            "MicroVM requires operator SSH, tar with --exclude, flock with timed locks, and util-linux setpriv with direct no-new-privileges UID switching",
        ));
    }
    let work = attempt.join("work");
    let archive = tokio::task::spawn_blocking(move || workspace::archive(&work))
        .await
        .map_err(io::Error::other)??;
    let stage_command = ssh(state, &binding, &attempt)?;
    super::access::attempt(state, saved).map_err(io::Error::other)?;
    saved.guest_pending = true;
    saved.guest_staged = true;
    super::save(state, saved).await.map_err(io::Error::other)?;
    let staged = capture(
        stage_command,
        script(
            "stage",
            saved.task.attempt_id,
            None,
            &STANDARD.encode(archive),
            0,
        ),
        65536,
        Duration::from_secs(60),
    )
    .await?;
    if staged.code != 0 {
        return Err(io::Error::other(
            "MicroVM project staging failed; verify SSH, tar, an unprivileged-user utility and the injected toolbox",
        ));
    }
    let invocation = Uuid::new_v4();
    super::access::attempt(state, saved).map_err(io::Error::other)?;
    saved.guest_command = Some(invocation);
    super::save(state, saved).await.map_err(io::Error::other)?;
    let output = capture(
        ssh(state, &binding, &attempt)?,
        script(
            "run",
            saved.task.attempt_id,
            Some(invocation),
            &STANDARD.encode(command),
            seconds,
        ),
        65536,
        Duration::from_secs(seconds + 15),
    )
    .await?;
    let status = capture(
        ssh(state, &binding, &attempt)?,
        script("status", saved.task.attempt_id, Some(invocation), "", 0),
        1024,
        Duration::from_secs(30),
    )
    .await?;
    if status.code != 0 || String::from_utf8_lossy(&status.stdout).trim() != output.code.to_string()
    {
        return Err(io::Error::other(
            "Guest invocation completion could not be confirmed; reconcile before retrying",
        ));
    }
    export(state, saved).await?;
    Ok(output)
}

/// Stop only this attempt's root supervisor and dedicated guest UID, then verify termination.
pub async fn cancel(state: &AppState, saved: &SavedTask) -> io::Result<()> {
    let Some(binding) = &saved.guest else {
        return Ok(());
    };
    let attempt = attempt(saved)?;
    let output = capture(
        ssh(state, binding, &attempt)?,
        script("cancel", saved.task.attempt_id, saved.guest_command, "", 0),
        65536,
        Duration::from_secs(30),
    )
    .await?;
    if output.code != 0 {
        return Err(io::Error::other("Guest termination could not be confirmed"));
    }
    Ok(())
}

/// Recover a complete bounded workspace from the authenticated guest.
pub async fn export(state: &AppState, saved: &SavedTask) -> io::Result<()> {
    let binding = saved
        .guest
        .as_ref()
        .ok_or_else(|| io::Error::other("No guest binding"))?;
    let attempt = attempt(saved)?;
    let result = capture(
        ssh(state, binding, &attempt)?,
        script("export", saved.task.attempt_id, saved.guest_command, "", 0),
        super::files::TREE_LIMIT + 4 * 1024 * 1024,
        Duration::from_secs(60),
    )
    .await?;
    if result.code == 79 {
        return Ok(());
    }
    if result.code != 0 {
        return Err(io::Error::other(
            "Guest export failed; guest workspace was retained",
        ));
    }
    let work = attempt.join("work");
    tokio::task::spawn_blocking(move || workspace::import(&work, &result.stdout))
        .await
        .map_err(io::Error::other)?
}

/// Remove only attempt-owned guest data after a successful export and termination check.
pub async fn cleanup(state: &AppState, saved: &SavedTask) -> io::Result<()> {
    let Some(binding) = &saved.guest else {
        return Ok(());
    };
    let attempt = attempt(saved)?;
    let output = capture(
        ssh(state, binding, &attempt)?,
        script("cleanup", saved.task.attempt_id, saved.guest_command, "", 0),
        65536,
        Duration::from_secs(30),
    )
    .await?;
    if output.code != 0 {
        return Err(io::Error::other(
            "Guest workspace cleanup requires reconciliation",
        ));
    }
    Ok(())
}

// All substitutions except the base64 payload are generated UUIDs, fixed actions or bounded integers.
// The root-owned control directory is outside the command user's writable workspace.
const SCRIPT: &str = r#"set -eu
umask 077
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PATH
id='__ID__'
invocation='__COMMAND__'
action='__ACTION__'
user="a$(printf '%s' "$id" | cut -c1-30)"
root=/var/lib/firecrab-ai
meta="$root/.control"
fence="$meta/fences/$id"
base="$root/$id"
control="$base/control"
work="$base/workspace"
cmd="$control/commands/$invocation"
tool() { if [ -x /etc/firecrab/busybox ]; then /etc/firecrab/busybox "$@"; else "$@"; fi; }
[ "$(id -u)" = 0 ] || exit 80
if command -v setpriv >/dev/null 2>&1 && setpriv --reuid 0 --regid 0 --clear-groups --no-new-privs /bin/true 2>/dev/null; then
  drop_program=$(command -v setpriv)
else printf 'Sandbox requires util-linux setpriv with direct UID switching and no-new-privileges. Install util-linux in the selected development VM.\n' >&2; exit 80; fi
if command -v flock >/dev/null 2>&1; then lock_program=$(command -v flock)
else lock_program=/etc/firecrab/busybox; fi
lock() { if [ "$lock_program" = /etc/firecrab/busybox ]; then "$lock_program" flock "$@"; else "$lock_program" "$@"; fi; }
lock -w 0 --help >/dev/null 2>&1 || exit 80
if command -v tar >/dev/null 2>&1 && tar --exclude=__firecrab_probe__ --help >/dev/null 2>&1; then tar_program=$(command -v tar)
elif [ -x /etc/firecrab/busybox ] && /etc/firecrab/busybox tar --exclude=__firecrab_probe__ --help >/dev/null 2>&1; then tar_program=/etc/firecrab/busybox
else printf 'Sandbox requires tar with --exclude support. Install GNU tar in the selected development VM.\n' >&2; exit 80; fi
archive_tar() { if [ "$tar_program" = /etc/firecrab/busybox ]; then "$tar_program" tar "$@"; else "$tar_program" "$@"; fi; }
[ "$action" != probe ] || exit 0
mkdir -p "$meta/locks" "$meta/fences"
chmod 711 "$root" "$meta" "$meta/fences"
chmod 700 "$meta/locks"
exec 9>"$meta/locks/$id"
lock -w 10 9 || exit 81
case "$action" in stage|run) [ ! -e "$fence" ] || exit 71;; esac
mkdir -p "$control"
chmod 711 "$base"
if [ -f "$control/owner" ]; then [ "$(cat "$control/owner")" = "$id" ] || exit 70; fi
printf '%s' "$id" >"$control/owner"
boot=$(cat /proc/sys/kernel/random/boot_id)
if [ -f "$control/uid" ]; then
  [ "$(id -u "$user" 2>/dev/null || true)" = "$(cat "$control/uid")" ] || exit 72
elif id -u "$user" >/dev/null 2>&1; then exit 72; fi
terminate_user() {
  uid=$(id -u "$user" 2>/dev/null) || return 0
  for status in /proc/[0-9]*/status; do
    [ -r "$status" ] || continue
    actual=$(tool awk '/^Uid:/ {print $2; exit}' "$status" 2>/dev/null) || continue
    [ "$actual" = "$uid" ] || continue
    pid=${status#/proc/}; pid=${pid%/status}
    kill -KILL "$pid" 2>/dev/null || true
  done
}
user_alive() {
  uid=$(id -u "$user" 2>/dev/null) || return 1
  for status in /proc/[0-9]*/status; do
    [ -r "$status" ] || continue
    actual=$(tool awk '/^Uid:/ {print $2; exit}' "$status" 2>/dev/null) || continue
    state=$(tool awk '/^State:/ {print $2; exit}' "$status" 2>/dev/null) || continue
    if [ "$actual" = "$uid" ] && [ "$state" != Z ]; then return 0; fi
  done
  return 1
}
stop_user() {
  for turn in 1 2 3 4 5 6 7 8 9 10; do
    terminate_user
    if ! user_alive; then return 0; fi
    tool sleep 0.1
  done
  return 1
}
current_complete() {
  [ -f "$control/current" ] || return 0
  current=$(cat "$control/current")
  case "$current" in *[!0-9a-f]*|'') return 1;; esac
  [ "${#current}" = 32 ] || return 1
  [ -f "$control/commands/$current/finished" ]
}
drop_user() {
  "$drop_program" --reuid "$(id -u "$user")" --regid "$(id -g "$user")" --clear-groups --no-new-privs env -i HOME="$work" PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin /bin/sh "$1"
}
case "$action" in
stage)
  current_complete && ! user_alive || exit 76
  if [ -f "$control/boot" ]; then [ "$(cat "$control/boot")" = "$boot" ] || exit 77; fi
  if ! id -u "$user" >/dev/null 2>&1; then
    if command -v useradd >/dev/null 2>&1; then useradd -M -U -d "$work" -s /bin/sh "$user"
    elif command -v adduser >/dev/null 2>&1; then adduser -D -H -h "$work" -s /bin/sh "$user"
    else tool adduser -D -H -h "$work" -s /bin/sh "$user"; fi
  fi
  [ "$(id -gn "$user")" = "$user" ] || exit 72
  id -u "$user" >"$control/uid"
  chgrp "$user" "$control"
  chmod 710 "$control"
  mkdir -p "$work"
  tool base64 -d >"$control/project.tar" <<'FIRECRAB_PAYLOAD'
__PAYLOAD__
FIRECRAB_PAYLOAD
  tool tar -tf "$control/project.tar" >"$control/next-files"
  [ -f "$control/previous-files" ] || touch "$control/previous-files"
  cat >"$control/refresh" <<FIRECRAB_REFRESH
set -eu
work='$work'
control='$control'
tool() { if [ -x /etc/firecrab/busybox ]; then /etc/firecrab/busybox "\$@"; else "\$@"; fi; }
while IFS= read -r file; do
  if ! tool grep -Fqx -- "\$file" "\$control/next-files"; then rm -f -- "\$work/\$file"; fi
done <"\$control/previous-files"
tool tar -xf "\$control/project.tar" -C "\$work"
FIRECRAB_REFRESH
  chgrp "$user" "$control/project.tar" "$control/previous-files" "$control/next-files" "$control/refresh"
  chmod 640 "$control/project.tar" "$control/previous-files" "$control/next-files" "$control/refresh"
  chown "$user:$user" "$work"
  chmod 700 "$work"
  drop_user "$control/refresh" 9>&-
  cp "$control/next-files" "$control/previous-files"
  rm -f "$control/project.tar"
  printf '%s' "$boot" >"$control/boot"
  touch "$control/staged"
  ;;
run)
  [ -f "$control/staged" ] || exit 79
  [ "$(cat "$control/boot")" = "$boot" ] || exit 77
  current_complete && ! user_alive || exit 76
  [ ! -e "$cmd" ] || exit 82
  mkdir -p "$control/commands"
  mkdir "$cmd"
  chgrp "$user" "$control/commands" "$cmd"
  chmod 710 "$control/commands" "$cmd"
  tool base64 -d >"$cmd/command" <<'FIRECRAB_PAYLOAD'
__PAYLOAD__
FIRECRAB_PAYLOAD
  printf '#!/bin/sh\nset -eu\n[ ! -e %s ] || exit 71\ncd %s\nexec /bin/sh %s/command\n' "$fence" "$work" "$cmd" >"$cmd/start"
  chgrp "$user" "$cmd/command" "$cmd/start"
  chmod 640 "$cmd/command" "$cmd/start"
  printf '%s %s\n' "$$" "$(tool awk '{print $22}' /proc/$$/stat)" >"$control/supervisor"
  printf '%s' "$invocation" >"$control/current"
  trap 'terminate_user' HUP INT TERM
  trap 'terminate_user' EXIT
  printf '#!/bin/sh\nulimit -n 128\nulimit -f 65536\nulimit -t 120\n' >"$cmd/launch"
  printf 'exec %s --reuid %s --regid %s --clear-groups --no-new-privs env -i HOME=%s PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin /bin/sh %s/start\n' "$drop_program" "$(id -u "$user")" "$(id -g "$user")" "$work" "$cmd" >>"$cmd/launch"
  tool mkfifo "$cmd/outpipe" "$cmd/errpipe"
  exec 9>&-
  ( tool head -c 65537 <"$cmd/outpipe" >"$cmd/stdout"; count=$(tool wc -c <"$cmd/stdout"); if [ "$count" -gt 65536 ]; then touch "$cmd/output-limit"; terminate_user; fi ) & out_reader=$!
  ( tool head -c 65537 <"$cmd/errpipe" >"$cmd/stderr"; count=$(tool wc -c <"$cmd/stderr"); if [ "$count" -gt 65536 ]; then touch "$cmd/output-limit"; terminate_user; fi ) & err_reader=$!
  set +e
  tool timeout -s KILL '__SECONDS__' /bin/sh "$cmd/launch" >"$cmd/outpipe" 2>"$cmd/errpipe" & launcher=$!
  while :; do
    wait "$launcher"; code=$?
    if ! kill -0 "$launcher" 2>/dev/null; then break; fi
  done
  stop_user || exit 76
  wait "$out_reader" "$err_reader" 2>/dev/null || true
  if [ -f "$cmd/output-limit" ]; then code=125; fi
  printf '%s' "$code" >"$cmd/exit"
  touch "$cmd/finished"
  tool head -c 65536 "$cmd/stdout"
  if [ -f "$cmd/output-limit" ]; then
    tool head -c 65280 "$cmd/stderr" >&2
    printf '\nSandbox output exceeded 64 KiB per stream; the owned command was stopped.\n' >&2
  else tool head -c 65536 "$cmd/stderr" >&2; fi
  exit "$code"
  ;;
cancel|cleanup)
  touch "$fence"
  same_boot=false
  if [ "$(cat "$control/boot" 2>/dev/null || true)" = "$boot" ]; then same_boot=true; fi
  if [ "$same_boot" = true ] && [ -f "$control/supervisor" ]; then
    read -r pid start <"$control/supervisor"
    case "$pid:$start" in *[!0-9:]*|'') exit 73;; esac
    if [ -r "/proc/$pid/stat" ] && [ "$(tool awk '{print $22}' "/proc/$pid/stat")" = "$start" ]; then kill -TERM "$pid" 2>/dev/null || true; fi
  fi
  confirmed=false
  for turn in 1 2 3 4 5 6 7 8 9 10; do
    terminate_user
    if ! user_alive && { [ "$same_boot" = false ] || current_complete; }; then confirmed=true; break; fi
    tool sleep 0.2
  done
  [ "$confirmed" = true ] || exit 76
  if [ "$action" = cleanup ]; then
    if id -u "$user" >/dev/null 2>&1; then
      if command -v userdel >/dev/null 2>&1; then userdel "$user"; else tool deluser "$user"; fi
    fi
    rm -rf "$base"
  fi
  ;;
status)
  [ -f "$cmd/finished" ] && ! user_alive || exit 78
  cat "$cmd/exit"
  ;;
export)
  [ -f "$control/staged" ] || exit 79
  { [ "$(cat "$control/boot")" != "$boot" ] || current_complete; } && ! user_alive || exit 76
  [ -d "$work" ] || exit 74
  archive_tar -cf - --exclude=node_modules --exclude=target --exclude=.git --exclude=.venv --exclude=venv --exclude=.next --exclude=dist --exclude=coverage --exclude=.coverage --exclude=.cache --exclude=.npm --exclude=.pnpm-store --exclude=.rustup --exclude=__pycache__ --exclude=.pytest_cache --exclude=.mypy_cache --exclude=.ruff_cache --exclude='.cargo/registry' --exclude='.cargo/git' --exclude='.yarn/cache' --exclude=.ssh --exclude=.aws --exclude=.codex --exclude=.claude --exclude='.env*' --exclude='*.key' --exclude='*.pem' -C "$work" .
  ;;
*) exit 75;;
esac
"#;

#[cfg(test)]
#[path = "guest_tests.rs"]
mod tests;
