import { useRef, useState, type ReactNode } from "react";
import type { PortForward, VmResponse } from "../bindings";
import { downloadSshKey, fetchSshKeyPem, updateVmPortForwards } from "../api/client";
import { isValidPort } from "../lib/portForward";
import { copyText } from "../lib/textExport";
import { useI18n } from "../i18n";

function pemName(vm: VmResponse): string {
  const safe = vm.name.replace(/[^A-Za-z0-9._-]+/g, "-") || "vm";
  return `firecrab-${safe}.pem`;
}

function sshCommandRows(vm: VmResponse): { label: string; command: string }[] {
  const pem = pemName(vm);
  const rows: { label: string; command: string }[] = [];
  if (vm.ipv4) {
    rows.push({ label: "IPv4", command: `ssh -i ${pem} root@${vm.ipv4}` });
  }
  if (vm.ipv6) {
    rows.push({ label: "IPv6", command: `ssh -6 -i ${pem} root@${vm.ipv6}` });
  }
  return rows;
}

/**
 * The button's download, written out for a shell on the Firecrab host — the
 * useful form when the dashboard is open on another machine. `-O` names the
 * file the `ssh -i` rows expect, and the `chmod` is not decoration: ssh refuses
 * a key the rest of the host can read.
 */
function wgetCommand(vm: VmResponse): string {
  const pem = pemName(vm);
  const base = window.location.origin;
  return `wget -O ${pem} ${base}/api/vms/${vm.id}/ssh-key && chmod 600 ${pem}`;
}

/** The `guest 22/tcp` rule this panel owns, if the operator made one. */
function sshPortForward(forwards: PortForward[]): PortForward | undefined {
  return forwards.find((pf) => pf.guestPort === 22 && pf.protocol === "tcp");
}

/**
 * Stand-in for the host address in commands meant to run somewhere else. The
 * address the dashboard is served from is often not the one that reaches this
 * host from a laptop — `localhost` through a tunnel, a LAN IP from outside —
 * so the operator fills this in rather than copying something wrong.
 */
const HOST_PLACEHOLDER = "<hostIP>";

/**
 * The jump host's own account. Without it ssh falls back to the username on
 * the machine the command runs from, which is rarely the account on the
 * Firecrab host — a non-standard host SSH port goes here too, as `…:2222`.
 */
const HOST_LOGIN_PLACEHOLDER = `<hostUser>@${HOST_PLACEHOLDER}`;

/** Reaching the guest through a forward the operator asked for. */
function forwardedCommand(vm: VmResponse, hostPort: number): string {
  return `ssh -p ${hostPort} -i ${pemName(vm)} root@${HOST_PLACEHOLDER}`;
}

/**
 * The same guest through the Firecrab host as a jump box. This needs no
 * firewall rule at all — the host already reaches the guest — so it works
 * from a laptop while inbound stays denied.
 */
function proxyJumpCommand(vm: VmResponse): string | null {
  if (!vm.ipv4) return null;
  return `ssh -J ${HOST_LOGIN_PLACEHOLDER} -i ${pemName(vm)} root@${vm.ipv4}`;
}

function verifyCommandRows(vm: VmResponse): { label: string; command: string }[] {
  const rows: { label: string; command: string }[] = [];
  if (vm.ipv4) {
    rows.push({
      label: "verify ipv4",
      command: `ssh-keyscan -t ed25519 ${vm.ipv4} | ssh-keygen -lf -`,
    });
  }
  if (vm.ipv6) {
    rows.push({
      label: "verify ipv6",
      command: `ssh-keyscan -6 -t ed25519 ${vm.ipv6} | ssh-keygen -lf -`,
    });
  }
  return rows;
}

/**
 * One-liners that compare for the operator and print `MATCH` or `MISMATCH`,
 * so nobody has to read two base64 fingerprints side by side.
 *
 * `grep -qF` keeps the fingerprint a literal: base64 carries `+` and `/`,
 * which a pattern would otherwise reinterpret.
 */
function checkCommandRows(
  vm: VmResponse,
  expected: string,
): { label: string; command: string }[] {
  if (!expected) return [];
  const decide = (scan: string) =>
    `${scan} 2>/dev/null | ssh-keygen -lf - | grep -qF '${expected}' && echo MATCH || echo MISMATCH`;
  const rows: { label: string; command: string }[] = [];
  if (vm.ipv4) {
    rows.push({
      label: "check ipv4",
      command: decide(`ssh-keyscan -t ed25519 ${vm.ipv4}`),
    });
  }
  if (vm.ipv6) {
    rows.push({
      label: "check ipv6",
      command: decide(`ssh-keyscan -6 -t ed25519 ${vm.ipv6}`),
    });
  }
  return rows;
}

/** Eye / eye-off, drawn like `NavIcons.tsx` (24x24, tracks `currentColor`). */
function EyeIcon({ off }: { off: boolean }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      className="console-ssh-eye"
      aria-hidden="true"
    >
      <path d="M1.8 12S5.4 5.4 12 5.4 22.2 12 22.2 12 18.6 18.6 12 18.6 1.8 12 1.8 12Z" />
      <circle cx="12" cy="12" r="3.1" />
      {off && <line x1="3.8" y1="20.2" x2="20.2" y2="3.8" />}
    </svg>
  );
}

/** Stand-in shown until the eye is opened: the envelope, never the key. */
const MASKED_KEY = [
  "-----BEGIN OPENSSH PRIVATE KEY-----",
  "•".repeat(44),
  "•".repeat(44),
  "•".repeat(44),
  "-----END OPENSSH PRIVATE KEY-----",
].join("\n");

function CopyableBlock({
  label,
  command,
  action,
}: {
  label: string;
  command: string;
  /** Rendered after Copy — used by the port-forward block for its Remove. */
  action?: ReactNode;
}) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const onCopy = async () => {
    const ok = await copyText(command);
    if (timer.current !== null) clearTimeout(timer.current);
    setCopied(ok);
    timer.current = setTimeout(() => setCopied(false), 2_000);
  };

  return (
    <div className="console-ssh-block">
      <div className="console-ssh-block-head">
        <span className="console-ssh-block-label">{label}</span>
        <button type="button" className="btn console-bar-btn" onClick={() => void onCopy()}>
          {copied ? t("Copied", "복사됨") : t("Copy", "복사")}
        </button>
        {action}
      </div>
      <pre className="console-ssh-code">
        <code>{command}</code>
      </pre>
    </div>
  );
}

interface ConsoleSshTabProps {
  vm: VmResponse | null;
}

/** Copyable `ssh -i` commands, PEM download, and a self-deciding host-key check. Not a live SSH client. */
export default function ConsoleSshTab({ vm }: ConsoleSshTabProps) {
  const { t } = useI18n();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [keyCopied, setKeyCopied] = useState(false);
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** PEM body, fetched only once the operator asks to see or copy it. */
  const [keyText, setKeyText] = useState<string | null>(null);
  const [keyShown, setKeyShown] = useState(false);
  /**
   * Port forwards as this panel last wrote them, so the block flips without
   * waiting for the parent's next poll. Tagged with the VM it belongs to:
   * the same component is reused when another VM's panel opens.
   */
  const [written, setWritten] = useState<{ id: string; forwards: PortForward[] } | null>(null);
  const [hostPort, setHostPort] = useState("22022");

  if (!vm) {
    return (
      <div className="console-ssh">
        <p className="console-ssh-empty">{t("Loading…", "불러오는 중…")}</p>
      </div>
    );
  }

  const pem = pemName(vm);
  const forwards = written?.id === vm.id ? written.forwards : (vm.portForwards ?? []);
  const forwarded = sshPortForward(forwards);
  const jump = proxyJumpCommand(vm);
  const expected = vm.sshHostFingerprint ?? "";
  const loginRows = sshCommandRows(vm);
  const verifyRows = verifyCommandRows(vm);
  const checkRows = checkCommandRows(vm, expected);

  const onDownload = () => {
    setError(null);
    setBusy(true);
    downloadSshKey(vm.id, vm.name)
      .catch((err: unknown) => {
        setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => setBusy(false));
  };

  // The private key reaches the browser only when asked for, and is kept for
  // the rest of the session so the eye and the copy button share one fetch.
  const loadKey = async (): Promise<string> => {
    if (keyText !== null) return keyText;
    const pem = await fetchSshKeyPem(vm.id);
    setKeyText(pem);
    return pem;
  };

  const onToggleKey = async () => {
    if (keyShown) {
      setKeyShown(false);
      return;
    }
    setError(null);
    setBusy(true);
    try {
      await loadKey();
      setKeyShown(true);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  // The PEM never lands on disk this way — handy on a host reached through a
  // browser, where the download would sit on the wrong machine.
  const onCopyKey = async () => {
    setError(null);
    setBusy(true);
    try {
      const copied = await copyText(await loadKey());
      if (copyTimer.current !== null) clearTimeout(copyTimer.current);
      setKeyCopied(copied);
      copyTimer.current = setTimeout(() => setKeyCopied(false), 2_000);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  // Both directions go through the same endpoint the detail modal uses, which
  // replaces the whole list — so the other rules have to be sent back with it.
  const writeForwards = async (next: PortForward[]) => {
    setError(null);
    setBusy(true);
    try {
      const updated = await updateVmPortForwards(vm.id, { portForwards: next });
      setWritten({ id: vm.id, forwards: updated.portForwards ?? [] });
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const onCreateForward = async () => {
    const port = Number(hostPort);
    if (!isValidPort(port)) {
      setError(t("Host port must be 1-65535.", "호스트 포트는 1-65535여야 합니다."));
      return;
    }
    await writeForwards([...forwards, { hostPort: port, guestPort: 22, protocol: "tcp" }]);
  };

  const onRemoveForward = async () => {
    await writeForwards(forwards.filter((pf) => pf !== forwarded));
  };

  return (
    <div className="console-ssh">
      <p className="console-ssh-lead">
        {t(
          "Download the PEM, copy a command, run it on the Firecrab host. SHA256 is the guest host key — not the PEM.",
          "PEM을 받고 명령을 복사해 Firecrab 호스트에서 실행하세요. SHA256은 게스트 호스트 키입니다. PEM이 아닙니다.",
        )}
      </p>
      <div className="console-ssh-actions">
        <button type="button" className="btn console-bar-btn" disabled={busy} onClick={onDownload}>
          {busy ? t("Downloading…", "받는 중…") : t(`Download ${pem}`, `${pem} 다운로드`)}
        </button>
      </div>
      {error ? <p className="field-error">{error}</p> : null}

      <CopyableBlock label="wget" command={wgetCommand(vm)} />

      <div className="console-ssh-block">
        <div className="console-ssh-block-head">
          <span className="console-ssh-block-label">{pem}</span>
          <button
            type="button"
            className="btn console-bar-btn console-ssh-eye-btn"
            disabled={busy}
            aria-pressed={keyShown}
            aria-label={keyShown ? t("Hide key", "키 가리기") : t("Show key", "키 보기")}
            title={keyShown ? t("Hide key", "키 가리기") : t("Show key", "키 보기")}
            onClick={() => void onToggleKey()}
          >
            <EyeIcon off={keyShown} />
          </button>
          <button
            type="button"
            className="btn console-bar-btn"
            disabled={busy}
            onClick={() => void onCopyKey()}
            title={t("Copy the private key text", "개인 키 본문을 클립보드로 복사")}
          >
            {keyCopied ? t("Key copied", "키 복사됨") : t("Copy key", "키 복사")}
          </button>
        </div>
        <pre className={`console-ssh-code${keyShown ? "" : " is-masked"}`}>
          <code>{keyShown && keyText !== null ? keyText.trimEnd() : MASKED_KEY}</code>
        </pre>
      </div>

      {expected ? (
        <CopyableBlock label="fingerprint" command={expected} />
      ) : (
        <p className="console-ssh-empty">
          {t("Host fingerprint after first start.", "호스트 지문은 첫 시작 후 표시됩니다.")}
        </p>
      )}

      {verifyRows.map((row) => (
        <CopyableBlock key={row.label} label={row.label} command={row.command} />
      ))}

      {checkRows.map((row) => (
        <CopyableBlock key={row.label} label={row.label} command={row.command} />
      ))}

      {jump ? <CopyableBlock label="proxy jump" command={jump} /> : null}

      {forwarded ? (
        <CopyableBlock
          label="port forward"
          command={forwardedCommand(vm, forwarded.hostPort)}
          action={
            <button
              type="button"
              className="btn console-bar-btn"
              disabled={busy}
              onClick={() => void onRemoveForward()}
            >
              {t("Remove SSH port forward", "SSH 포트 포워드 제거")}
            </button>
          }
        />
      ) : (
        <div className="console-ssh-forward">
          <label htmlFor="ssh-forward-host-port">{t("host port", "호스트 포트")}</label>
          <input
            id="ssh-forward-host-port"
            type="number"
            min={1}
            max={65535}
            value={hostPort}
            disabled={busy}
            onChange={(event) => setHostPort(event.target.value)}
          />
          <span className="console-ssh-forward-target">→ guest 22/tcp</span>
          <button
            type="button"
            className="btn console-bar-btn"
            disabled={busy}
            onClick={() => void onCreateForward()}
          >
            {t("Create SSH port forward", "SSH 포트 포워드 만들기")}
          </button>
        </div>
      )}
      <p className="console-ssh-empty">
        {t(
          "A forward opens the port on this host only — reaching it from outside also needs the router to forward it.",
          "포워드는 이 호스트의 포트만 엽니다. 외부에서 닿으려면 공유기에서도 해당 포트를 넘겨야 합니다.",
        )}
      </p>

      {loginRows.length === 0 ? (
        <p className="console-ssh-empty">
          {t("No address yet — start the VM.", "주소가 없습니다. VM을 시작하세요.")}
        </p>
      ) : (
        loginRows.map((row) => (
          <CopyableBlock key={row.label} label={row.label} command={row.command} />
        ))
      )}
    </div>
  );
}
