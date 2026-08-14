#!/usr/bin/env node
/**
 * Start firecrab-api from the repo root so Playwright can drive the dashboard.
 *
 * Prepares the Ubuntu catalog kernel the OCI import stage pairs with (symlink
 * only if missing) and points FIRECRAB_OCI_TOOLBOX_PATH at a local static
 * busybox when present so the import does not pull from Docker Hub.
 *
 * Playwright's webServer `reuseExistingServer` skips this script when
 * http://127.0.0.1:3000/api/host already answers.
 */
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../..");

function hostKernelRelPath() {
  const machine = os.machine();
  if (machine === "aarch64" || machine === "arm64") {
    return "kernel/Image-ubuntu-26.04-aarch64";
  }
  return "kernel/vmlinux-ubuntu-26.04-x86_64";
}

function isUsableKernel(file) {
  try {
    const st = fs.lstatSync(file);
    // Import opens the kernel with O_NOFOLLOW; a symlink is refused.
    return st.isFile() && st.size > 0;
  } catch {
    return false;
  }
}

function ensureKernel() {
  const rel = hostKernelRelPath();
  const dest = path.join(repoRoot, "images", rel);
  if (isUsableKernel(dest)) return dest;
  try {
    fs.unlinkSync(dest);
  } catch {
    /* dest missing or not removable */
  }
  const candidates = [path.join(repoRoot, "docs/microregistry/images", rel)];
  const source = candidates.find((candidate) => isUsableKernel(candidate));
  if (!source) {
    console.warn(
      `[firecrab-e2e] ${rel} is missing; OCI import will fail until the Ubuntu catalog kernel is installed`,
    );
    return null;
  }
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  try {
    fs.linkSync(source, dest);
    console.log(`[firecrab-e2e] hardlinked ${dest} <- ${source}`);
  } catch {
    fs.copyFileSync(source, dest);
    console.log(`[firecrab-e2e] copied ${dest} <- ${source}`);
  }
  return dest;
}

function isStaticBusybox(file) {
  try {
    const fd = fs.openSync(file, "r");
    const header = Buffer.alloc(64);
    fs.readSync(fd, header, 0, 64, 0);
    fs.closeSync(fd);
    return header[0] === 0x7f && header[1] === 0x45 && header[2] === 0x4c && header[3] === 0x46;
  } catch {
    return false;
  }
}

ensureKernel();

const env = { ...process.env };
if (!env.FIRECRAB_OCI_TOOLBOX_PATH && isStaticBusybox("/usr/bin/busybox")) {
  env.FIRECRAB_OCI_TOOLBOX_PATH = "/usr/bin/busybox";
}

const cargo = spawn("cargo", ["run", "-p", "firecrab-api"], {
  cwd: repoRoot,
  env,
  stdio: "inherit",
});
cargo.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 1);
});
