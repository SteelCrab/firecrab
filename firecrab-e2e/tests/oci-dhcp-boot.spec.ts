import { execFile } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { arch, tmpdir } from "node:os";
import path from "node:path";
import { promisify } from "node:util";

import { expect, test, type Page } from "@playwright/test";

import { ApiCleanup } from "../src/api.js";
import {
  apiUrl,
  DHCP_FIXED_ALIAS,
  DHCP_FIXED_REFERENCE,
  DHCP_HOST_PORT,
  DHCP_SSH_HOST_PORT,
  DHCP_NETWORK_CIDR,
  DHCP_NETWORK_NAME,
  DHCP_REGISTRY_PORT,
  DHCP_VM_NAME,
  NETWORK_FAILED,
  NETWORK_READY,
  SKIP_GUEST_BOOT,
} from "../src/constants.js";
import { startLocalOciRegistry, type LocalOciRegistry } from "../src/registry.js";

/**
 * OCI guest dual-stack DHCP + SSH boot — the nginx-stable dashboard path
 * (busybox `udhcpc`, SLAAC, `FIRECRAB_NETWORK_READY`) without Docker Hub.
 *
 *   FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm --prefix firecrab-e2e run test:dhcp
 *   npm --prefix firecrab-e2e run test:dhcp
 *
 * Guest boot needs a helper the API can connect to
 * (`./scripts/dev-net-helper.sh`, socket `/run/firecrab/net-helper.sock`).
 * An orphan dnsmasq holding `:67` reproduces `FIRECRAB_NETWORK_FAILED
 * no-ipv4-address` — this spec fails loudly in that state.
 */
test.describe.configure({ mode: "serial" });

let registry: LocalOciRegistry;
let createdNetworkId: string | null = null;
const api = new ApiCleanup();
const execFileAsync = promisify(execFile);
const SSH_AUTH_READY = "FIRECRAB_SSH_AUTH_READY";

function reference(): string {
  return registry.announcement.reference;
}

function alias(): string {
  return registry.announcement.alias;
}

function sentinel(): string {
  return registry.announcement.ready;
}

function expectedOciArchitecture(): "amd64" | "arm64" {
  const machine = arch();
  if (machine === "x64") return "amd64";
  if (machine === "arm64") return "arm64";
  throw new Error(`unsupported OCI SSH E2E host architecture: ${machine}`);
}

async function openEnglish(page: Page, hash: string): Promise<void> {
  await page.addInitScript(() => {
    localStorage.setItem("firecrab.locale", "en");
  });
  await page.goto(hash);
}

async function cleanupOwned(): Promise<void> {
  const imported = registry?.announcement.alias ?? DHCP_FIXED_ALIAS;
  await api.deleteOwnedVms(imported, DHCP_VM_NAME);
  await api.deleteImportedImage(imported);
  await api.deleteNetworksByName([DHCP_NETWORK_NAME]);
  createdNetworkId = null;
}

async function authenticateWithDownloadedKey(vmId: string, ipv6: string): Promise<void> {
  const response = await fetch(`${apiUrl()}/api/vms/${vmId}/ssh-key`);
  expect(response.status, "download operator SSH key").toBe(200);
  const privateKey = await response.text();
  expect(privateKey).toContain("BEGIN OPENSSH PRIVATE KEY");

  const scratch = await mkdtemp(path.join(tmpdir(), "firecrab-e2e-ssh-"));
  const keyPath = path.join(scratch, `firecrab-${DHCP_VM_NAME}.pem`);
  try {
    await writeFile(keyPath, privateKey, { encoding: "utf8", mode: 0o600 });
    const authenticate = async (label: string, targetArgs: string[]): Promise<void> => {
      let lastError = "ssh did not run";
      let authenticatedOutput: string | null = null;
      const deadline = Date.now() + 60_000;
      while (Date.now() < deadline) {
        try {
          const { stdout } = await execFileAsync(
            "ssh",
            [
              "-i",
              keyPath,
              "-o",
              "BatchMode=yes",
              "-o",
              "ConnectTimeout=5",
              "-o",
              "IdentitiesOnly=yes",
              "-o",
              "StrictHostKeyChecking=no",
              "-o",
              "UserKnownHostsFile=/dev/null",
              "-o",
              "LogLevel=ERROR",
              ...targetArgs,
              `printf ${SSH_AUTH_READY}`,
            ],
            { timeout: 10_000 },
          );
          authenticatedOutput = stdout;
          break;
        } catch (error) {
          lastError = error instanceof Error ? error.message : String(error);
          await new Promise((resolve) => setTimeout(resolve, 1_000));
        }
      }
      if (authenticatedOutput === null) {
        const logResponse = await fetch(`${apiUrl()}/api/vms/${vmId}/log`);
        const logBody = (await logResponse.json()) as { consoleLog?: string };
        const consoleTail = (logBody.consoleLog ?? "").slice(-8_000);
        throw new Error(
          `${label} SSH key authentication did not become ready: ${lastError}\nGuest console tail:\n${consoleTail}`,
        );
      }
      expect(authenticatedOutput.trim()).toBe(SSH_AUTH_READY);
    };

    await authenticate("IPv4 port-forward", [
      "-p",
      String(DHCP_SSH_HOST_PORT),
      "root@127.0.0.1",
    ]);
    await authenticate("IPv6 direct", ["-6", `root@${ipv6}`]);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

test.beforeAll(async () => {
  registry = await startLocalOciRegistry(DHCP_REGISTRY_PORT);
  expect(registry.announcement.reference).toBe(DHCP_FIXED_REFERENCE);
  expect(registry.announcement.alias).toBe(DHCP_FIXED_ALIAS);
  expect(registry.announcement.architecture).toBe(expectedOciArchitecture());
  await cleanupOwned();
});

test.afterAll(async () => {
  try {
    await cleanupOwned();
  } finally {
    await registry?.stop();
  }
});

test("inspects the DHCP-boot fixture and imports it as a registered image", async ({ page }) => {
  await openEnglish(page, "/#/images");
  await expect(page.locator("#oci-reference")).toBeVisible();
  await expect(page.locator("#oci-import")).toBeDisabled();

  await page.locator("#oci-reference").fill(reference());
  await page.locator("#oci-inspect").click();

  const oci = page.locator("section.panel", { has: page.getByRole("heading", { name: "OCI" }) });
  await expect(oci.getByText("Compatible with this host.")).toBeVisible({ timeout: 30_000 });
  await expect(oci.locator("dd", { hasText: alias() }).first()).toBeVisible();
  await expect(page.locator("#oci-import")).toBeEnabled();

  await page.locator("#oci-import").click();
  const status = oci.locator(".state-badge");
  await expect(status).toHaveText(/Imported|Import failed/, { timeout: 180_000 });
  if ((await status.textContent())?.includes("failed")) {
    const log = (await oci.locator(".image-install-log").textContent()) ?? "";
    throw new Error(`OCI import failed for ${reference()}:\n${log}`);
  }

  await expect(page.locator("table.image-table")).toContainText(alias());
});

test("boots the imported dual-stack guest and authenticates SSH over IPv4 and IPv6", async ({
  page,
}) => {
  test.skip(
    SKIP_GUEST_BOOT,
    "FIRECRAB_E2E_SKIP_GUEST_BOOT is set — import already covered. Unset the flag (and run ./scripts/dev-net-helper.sh) to boot through DHCP.",
  );

  await openEnglish(page, "/#/networks");
  const networkPanel = page.locator("section.panel", {
    has: page.getByRole("heading", { name: "MicroNetwork" }),
  });
  const pendingNetwork = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" && response.url().includes("/api/micro-networks"),
  );
  await page.locator("#mn-name").fill(DHCP_NETWORK_NAME);
  await page.locator("#mn-subnet").fill(DHCP_NETWORK_CIDR);
  await networkPanel.locator("#mn-ipv6-enable").selectOption("on");
  await networkPanel.locator('button[type="submit"]').click();
  const networkResponse = await pendingNetwork;
  if (!networkResponse.ok()) {
    throw new Error(
      `POST /api/micro-networks ${networkResponse.status()}: ${await networkResponse.text()}`,
    );
  }
  const created = networkPanel
    .locator("table.vm-table tbody tr")
    .filter({ hasText: DHCP_NETWORK_NAME });
  await expect(created).toBeVisible({ timeout: 30_000 });
  const networks = await api.listNetworks();
  createdNetworkId = networks.find((network) => network.name === DHCP_NETWORK_NAME)?.id ?? null;
  expect(createdNetworkId, `API missing ${DHCP_NETWORK_NAME}`).toBeTruthy();
  expect(networks.find((network) => network.id === createdNetworkId)?.ipv6Cidr ?? "").toMatch(
    /^fd[0-9a-f:]+\/64$/i,
  );

  await openEnglish(page, "/#/vms");
  await page.locator("#vm-list-add").click();
  await expect(page).toHaveURL(/#\/vms\/new$/);
  await expect(page.locator("#vm-image")).toBeEnabled({ timeout: 15_000 });
  await expect(page.locator(`#vm-image option[value="${alias()}"]`)).toHaveCount(1, {
    timeout: 15_000,
  });
  await page.locator("#vm-name").fill(DHCP_VM_NAME);
  await page.locator("#vm-image").selectOption(alias());
  await page
    .locator("#vm-micro-network")
    .selectOption({ label: `${DHCP_NETWORK_NAME} (${DHCP_NETWORK_CIDR})` });
  await page.getByRole("button", { name: /Add Port Forward Rule/ }).click();
  const hostPort = page.locator(".port-forwards-list input[placeholder='8080']");
  await expect(hostPort).toHaveValue("8080");
  await hostPort.fill(String(DHCP_HOST_PORT));

  await page.locator("#vm-create-submit").click();
  await expect(page).toHaveURL(/#\/vms$/);
  await expect(page.getByText(`Created: ${DHCP_VM_NAME}`)).toBeVisible({ timeout: 30_000 });

  const row = page.locator("table.vm-table tbody tr", { hasText: DHCP_VM_NAME });
  await expect(row).toBeVisible();
  // Row actions live behind the Actions toggle now.
  await row.getByRole("button", { name: /^Actions$|^작업$/ }).click();
  await row.getByRole("menuitem", { name: "start" }).click();
  await expect(row.locator(".state-badge")).toHaveText(/running|error/, { timeout: 240_000 });
  if ((await row.locator(".state-badge").textContent()) !== "running") {
    await row.locator("button.link-button").click();
    await expect(page.locator(".console-title")).toBeVisible();
    const logText = (await page.locator("pre.detail-log").textContent()) ?? "";
    const banner = (await page.locator(".banner").textContent().catch(() => "")) ?? "";
    throw new Error(
      `VM ${DHCP_VM_NAME} entered error instead of running.\nbanner: ${banner}\nlog:\n${logText}`,
    );
  }

  await row.locator("button.link-button").click();
  const log = page.locator("pre.detail-log");
  await expect(log).toContainText(NETWORK_READY, { timeout: 30_000 });
  await expect(log).not.toContainText(NETWORK_FAILED);
  await expect(log).toContainText(sentinel(), { timeout: 60_000 });
  await expect(page.locator("dt").filter({ hasText: /^ip$/ }).locator("+ dd")).toContainText(
    /^172\.30\.94\./,
  );
  await expect(page.locator("dt", { hasText: /^ports$/ })).toBeVisible();
  await expect(page.getByText(`80:${DHCP_HOST_PORT}/tcp`)).toBeVisible();

  const vms = await api.listVms();
  const vm = vms.find((row) => row.name === DHCP_VM_NAME);
  expect(vm, `API missing ${DHCP_VM_NAME}`).toBeTruthy();
  const detail = await api.getVm(vm!.id);
  expect(detail?.state).toBe("running");
  expect(detail?.ipv4 ?? "").toMatch(/^172\.30\.94\./);
  expect(detail?.ipv6 ?? "").toMatch(/^fd[0-9a-f:]+$/i);
  expect(detail?.portForwards).toEqual(
    expect.arrayContaining([
      expect.objectContaining({
        guestPort: 80,
        hostPort: DHCP_HOST_PORT,
        protocol: "tcp",
      }),
    ]),
  );

  await expect(page.getByRole("button", { name: /Download key|키 다운로드/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /Copy key|키 복사/ })).toBeVisible();
  await page.locator(".console-panel > .console-bar .console-close").click();
  await expect(page.locator(".console-overlay")).toHaveCount(0);

  // SSH from the VM list: Actions → SSH connect opens a dialog with the panel.
  await openEnglish(page, "/#/vms");
  const sshRow = page.locator("table.vm-table tbody tr", { hasText: DHCP_VM_NAME });
  await sshRow.getByRole("button", { name: /^Actions$|^작업$/ }).click();
  await sshRow.getByRole("menuitem", { name: /SSH connect|SSH 연결/ }).click();
  const sshDialog = page.getByRole("dialog", { name: new RegExp(`SSH — ${DHCP_VM_NAME}`) });
  await expect(sshDialog.locator(".console-ssh-block-label", { hasText: /^fingerprint$/i })).toBeVisible();
  await sshDialog.getByRole("button", { name: "✕" }).click();
  await expect(sshDialog).toHaveCount(0);

  await openEnglish(page, `/#/console/${detail!.id}`);
  await page.getByRole("tab", { name: "SSH" }).click();
  await expect(page.locator(".console-ssh-block-label", { hasText: /^fingerprint$/i })).toBeVisible();
  // `check` is a copyable one-liner that decides instead of printing a fingerprint.
  await expect(page.locator(".console-ssh-block-label", { hasText: /^check ipv4$/ })).toBeVisible();
  await expect(page.locator(".console-ssh-block-label", { hasText: /^check ipv6$/ })).toBeVisible();
  await expect(
    page.locator(".console-ssh-code").filter({ hasText: "echo MATCH" }).first(),
  ).toContainText("ssh-keyscan -t ed25519");
  await expect(page.locator(".console-ssh-code").filter({ hasText: "ssh -i" }).first()).toContainText(
    `ssh -i firecrab-${DHCP_VM_NAME}.pem root@`,
  );
  await expect(
    page.locator(".console-ssh-code").filter({ hasText: "ssh -6 -i" }).first(),
  ).toContainText(`root@${detail!.ipv6}`);
  await expect(page.getByRole("button", { name: /Download |다운로드/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /Copy key|키 복사/ })).toBeVisible();

  // The key sits behind the eye until asked for.
  await expect(page.locator(".console-ssh-code.is-masked")).toHaveCount(1);
  await page.getByRole("button", { name: /Show key|키 보기/ }).click();
  await expect(page.locator(".console-ssh-code.is-masked")).toHaveCount(0);
  await page.getByRole("button", { name: /Hide key|키 가리기/ }).click();
  await expect(page.locator(".console-ssh-code.is-masked")).toHaveCount(1);

  // A jump through the Firecrab host needs no rule at all, so the command is
  // always there — the port forward below is the alternative, not the only way.
  await expect(
    page.locator(".console-ssh-code").filter({ hasText: "ssh -J" }).first(),
  ).toContainText(`root@${detail!.ipv4}`);

  // SSH port forward: the panel writes host:PORT → guest 22 through the same
  // endpoint the detail modal uses, and then prints the `ssh -p` command.
  await page.locator("#ssh-forward-host-port").fill(String(DHCP_SSH_HOST_PORT));
  await page.getByRole("button", { name: /Create SSH port forward|SSH 포트 포워드 만들기/ }).click();
  await expect(
    page.locator(".console-ssh-code").filter({ hasText: `ssh -p ${DHCP_SSH_HOST_PORT}` }).first(),
  ).toContainText(`-i firecrab-${DHCP_VM_NAME}.pem root@`);
  const forwarded = await api.getVm(detail!.id);
  expect(forwarded?.portForwards).toEqual(
    expect.arrayContaining([
      expect.objectContaining({
        guestPort: 22,
        hostPort: DHCP_SSH_HOST_PORT,
        protocol: "tcp",
      }),
    ]),
  );

  // Exercise the real contract, not only the dashboard text: guest sshd must
  // answer on 22/tcp and accept the per-VM key downloaded from the API.
  await authenticateWithDownloadedKey(detail!.id, detail!.ipv6!);

  // Removing takes back only the SSH rule; the guest-80 forward stays.
  await page.getByRole("button", { name: /Remove SSH port forward|SSH 포트 포워드 제거/ }).click();
  await expect(page.locator("#ssh-forward-host-port")).toBeVisible();
  const afterRemove = await api.getVm(detail!.id);
  expect(afterRemove?.portForwards?.some((pf) => pf.guestPort === 22)).toBe(false);
  expect(afterRemove?.portForwards?.some((pf) => pf.guestPort === 80)).toBe(true);
});
