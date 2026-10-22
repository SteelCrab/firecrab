import { useId, useRef, useState, type ReactNode } from "react";
import { Check, Copy, Download, Eye, EyeOff, KeyRound } from "lucide-react";
import type { PortForward, VmResponse } from "../../bindings";
import { downloadSshKey, fetchSshKeyPem, updateVmPortForwards } from "../../api/client";
import { isValidPort } from "../../lib/portForward";
import { copyText } from "../../lib/textExport";
import { useI18n } from "../../i18n";

function pemName(vm: VmResponse): string {
  const safe = vm.name.replace(/[^A-Za-z0-9._-]+/g, "-") || "vm";
  return `firecrab-${safe}.pem`;
}

interface CommandRow {
  label: string;
  command: string;
}

function sshCommandRows(vm: VmResponse): CommandRow[] {
  const pem = pemName(vm);
  const rows: CommandRow[] = [];
  if (vm.ipv4) rows.push({ label: "IPv4", command: `ssh -i ${pem} root@${vm.ipv4}` });
  if (vm.ipv6) rows.push({ label: "IPv6", command: `ssh -6 -i ${pem} root@${vm.ipv6}` });
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
  return `wget -O ${pem} ${window.location.origin}/api/vms/${vm.id}/ssh-key && chmod 600 ${pem}`;
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

function verifyCommandRows(vm: VmResponse): CommandRow[] {
  const rows: CommandRow[] = [];
  if (vm.ipv4) rows.push({ label: "verify ipv4", command: `ssh-keyscan -t ed25519 ${vm.ipv4} | ssh-keygen -lf -` });
  if (vm.ipv6) rows.push({ label: "verify ipv6", command: `ssh-keyscan -6 -t ed25519 ${vm.ipv6} | ssh-keygen -lf -` });
  return rows;
}

/**
 * One-liners that compare for the operator and print `MATCH` or `MISMATCH`,
 * so nobody has to read two base64 fingerprints side by side.
 *
 * `grep -qF` keeps the fingerprint a literal: base64 carries `+` and `/`,
 * which a pattern would otherwise reinterpret.
 */
function checkCommandRows(vm: VmResponse, expected: string): CommandRow[] {
  if (!expected) return [];
  const decide = (scan: string) =>
    `${scan} 2>/dev/null | ssh-keygen -lf - | grep -qF '${expected}' && echo MATCH || echo MISMATCH`;
  const rows: CommandRow[] = [];
  if (vm.ipv4) rows.push({ label: "check ipv4", command: decide(`ssh-keyscan -t ed25519 ${vm.ipv4}`) });
  if (vm.ipv6) rows.push({ label: "check ipv6", command: decide(`ssh-keyscan -6 -t ed25519 ${vm.ipv6}`) });
  return rows;
}

/** Stand-in shown until the eye is opened: the envelope, never the key. */
const MASKED_KEY = [
  "-----BEGIN OPENSSH PRIVATE KEY-----",
  "•".repeat(44),
  "•".repeat(44),
  "-----END OPENSSH PRIVATE KEY-----",
].join("\n");

/** Shows "Copied" for two seconds after a successful copy. */
function useCopied(): [copied: boolean, copy: (text: string | (() => Promise<string>)) => Promise<void>] {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const copy = async (text: string | (() => Promise<string>)) => {
    const ok = await copyText(typeof text === "string" ? text : await text());
    if (timer.current !== null) clearTimeout(timer.current);
    setCopied(ok);
    timer.current = setTimeout(() => setCopied(false), 2_000);
  };
  return [copied, copy];
}

function CopyableBlock({ label, command, action }: CommandRow & {
  /** Rendered after Copy — used by the port-forward block for its Remove. */
  action?: ReactNode;
}) {
  const { t } = useI18n();
  const [copied, copy] = useCopied();
  return (
    <div className="console-ssh-block">
      <div className="console-ssh-block-head">
        <span className="console-ssh-block-label">{label}</span>
        {action}
        <button
          type="button"
          className="btn sm ghost console-ssh-copy"
          aria-label={copied ? t("Copied", "복사됨") : t(`Copy ${label}`, `${label} 복사`)}
          onClick={() => void copy(command)}
        >
          {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
          {copied ? t("Copied", "복사됨") : t("Copy", "복사")}
        </button>
      </div>
      <pre className="console-ssh-code"><code>{command}</code></pre>
    </div>
  );
}

function SshStep({ index, title, hint, children }: { index: number; title: string; hint: string; children: ReactNode }) {
  const id = useId();
  return (
    <section className="ssh-step" aria-labelledby={id}>
      <div className="ssh-step-head">
        <span className="ssh-step-num" aria-hidden="true">{index}</span>
        <div>
          <h3 id={id} className="ssh-step-title">{title}</h3>
          <p className="ssh-step-hint">{hint}</p>
        </div>
      </div>
      <div className="ssh-step-body">{children}</div>
    </section>
  );
}

interface SshAccessProps {
  vm: VmResponse | null;
}

/**
 * Everything needed to reach a guest over SSH, in the order it is needed: the
 * key, the commands, the host-key check. Not a live SSH client. Shared by the
 * SSH dialog and the console page's SSH tab.
 */
export default function SshAccess({ vm }: SshAccessProps) {
  const { t } = useI18n();
  const [busy, setBusy] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [keyCopied, copyKey] = useCopied();
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
  const fail = (caught: unknown) => setError(caught instanceof Error ? caught.message : String(caught));

  const onDownload = () => {
    setError(null);
    setDownloading(true);
    downloadSshKey(vm.id, vm.name).catch(fail).finally(() => setDownloading(false));
  };

  // The private key reaches the browser only when asked for, and is kept for
  // the rest of the session so the eye and the copy button share one fetch.
  const loadKey = async (): Promise<string> => {
    if (keyText !== null) return keyText;
    const text = await fetchSshKeyPem(vm.id);
    setKeyText(text);
    return text;
  };

  const withBusy = async (work: () => Promise<void>) => {
    setError(null);
    setBusy(true);
    try {
      await work();
    } catch (caught: unknown) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const onToggleKey = () => {
    if (keyShown) return setKeyShown(false);
    void withBusy(async () => {
      await loadKey();
      setKeyShown(true);
    });
  };

  // The PEM never lands on disk this way — handy on a host reached through a
  // browser, where the download would sit on the wrong machine.
  const onCopyKey = () => void withBusy(() => copyKey(loadKey));

  // The endpoint replaces the whole list, so the other rules go back with it.
  const writeForwards = (next: PortForward[]) => withBusy(async () => {
    const updated = await updateVmPortForwards(vm.id, { portForwards: next });
    setWritten({ id: vm.id, forwards: updated.portForwards ?? [] });
  });

  const onCreateForward = () => {
    const port = Number(hostPort);
    if (!isValidPort(port)) {
      setError(t("Host port must be 1-65535.", "호스트 포트는 1-65535여야 합니다."));
      return;
    }
    void writeForwards([...forwards, { hostPort: port, guestPort: 22, protocol: "tcp" }]);
  };

  const keyLabel = keyShown ? t("Hide key", "키 가리기") : t("Show key", "키 보기");

  return (
    <div className="console-ssh">
      {error && <p className="field-error" role="alert">{error}</p>}

      <SshStep
        index={1}
        title={t("Private key", "개인 키")}
        hint={t("One key per VM. ssh refuses it unless only you can read it (chmod 600).", "VM마다 키가 하나입니다. 본인만 읽을 수 있어야(chmod 600) ssh가 받아들입니다.")}
      >
        <div className="console-ssh-block ssh-key">
          <div className="console-ssh-block-head">
            <KeyRound className="ssh-key-icon" aria-hidden="true" />
            <span className="console-ssh-block-label">{pem}</span>
            <button
              type="button"
              className="icon-btn"
              disabled={busy}
              aria-pressed={keyShown}
              aria-label={keyLabel}
              title={keyLabel}
              onClick={onToggleKey}
            >
              {keyShown ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}
            </button>
            <button type="button" className="btn sm" disabled={busy} title={t("Copy the private key text", "개인 키 본문을 클립보드로 복사")} onClick={onCopyKey}>
              {keyCopied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
              {keyCopied ? t("Key copied", "키 복사됨") : t("Copy key", "키 복사")}
            </button>
            <button type="button" className="btn sm" disabled={downloading} onClick={onDownload}>
              <Download aria-hidden="true" />
              {downloading ? t("Downloading…", "받는 중…") : t(`Download ${pem}`, `${pem} 다운로드`)}
            </button>
          </div>
          <pre className={`console-ssh-code${keyShown ? "" : " is-masked"}`}>
            <code>{keyShown && keyText !== null ? keyText.trimEnd() : MASKED_KEY}</code>
          </pre>
        </div>
        <CopyableBlock label="wget" command={wgetCommand(vm)} />
      </SshStep>

      <SshStep
        index={2}
        title={t("Connect", "접속")}
        hint={t("Run on the Firecrab host. From another machine, jump through the host or forward a port.", "Firecrab 호스트에서 실행합니다. 다른 컴퓨터에서는 호스트를 거치거나 포트를 넘깁니다.")}
      >
        {loginRows.length === 0 ? (
          <p className="console-ssh-empty">{t("No address yet — start the VM.", "주소가 없습니다. VM을 시작하세요.")}</p>
        ) : (
          loginRows.map((row) => <CopyableBlock key={row.label} {...row} />)
        )}
        {jump && <CopyableBlock label="proxy jump" command={jump} />}
        {forwarded ? (
          <CopyableBlock
            label="port forward"
            command={forwardedCommand(vm, forwarded.hostPort)}
            action={
              <button type="button" className="btn sm ghost" disabled={busy} onClick={() => void writeForwards(forwards.filter((pf) => pf !== forwarded))}>
                {t("Remove SSH port forward", "SSH 포트 포워드 제거")}
              </button>
            }
          />
        ) : (
          <div className="console-ssh-forward">
            <label htmlFor="ssh-forward-host-port">{t("Host port", "호스트 포트")}</label>
            <input
              id="ssh-forward-host-port"
              className="input"
              type="number"
              min={1}
              max={65535}
              value={hostPort}
              disabled={busy}
              onChange={(event) => setHostPort(event.target.value)}
            />
            <span className="console-ssh-forward-target">→ guest 22/tcp</span>
            <button type="button" className="btn sm" disabled={busy} onClick={onCreateForward}>
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
      </SshStep>

      <SshStep
        index={3}
        title={t("Verify host key", "호스트 키 확인")}
        hint={t("SHA256 is the guest host key — not the PEM. A check prints MATCH or MISMATCH.", "SHA256은 게스트 호스트 키입니다. PEM이 아닙니다. check는 MATCH 또는 MISMATCH를 출력합니다.")}
      >
        {expected ? (
          <CopyableBlock label="fingerprint" command={expected} />
        ) : (
          <p className="console-ssh-empty">{t("Host fingerprint after first start.", "호스트 지문은 첫 시작 후 표시됩니다.")}</p>
        )}
        {checkRows.map((row) => <CopyableBlock key={row.label} {...row} />)}
        {verifyRows.length > 0 && (
          <details className="ssh-more">
            <summary>{t("Print the fingerprint instead", "지문을 직접 출력하는 명령")}</summary>
            {verifyRows.map((row) => <CopyableBlock key={row.label} {...row} />)}
          </details>
        )}
      </SshStep>
    </div>
  );
}
