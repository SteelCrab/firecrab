import { spawn, type ChildProcess } from "node:child_process";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";

import { REGISTRY_PORT, REPO_ROOT } from "./constants.js";

export interface RegistryAnnouncement {
  reference: string;
  ready: string;
  alias: string;
  architecture: string;
}

export interface LocalOciRegistry {
  announcement: RegistryAnnouncement;
  stop(): Promise<void>;
}

function firstStdoutLine(child: ChildProcess, timeoutMs: number): Promise<string> {
  return new Promise((resolve, reject) => {
    const stdout = child.stdout;
    if (!stdout) {
      reject(new Error("oci-e2e-registry.py has no stdout pipe"));
      return;
    }
    let buffer = "";
    const timer = setTimeout(() => {
      cleanup();
      reject(
        new Error(
          `oci-e2e-registry.py did not print its JSON announcement within ${timeoutMs}ms`,
        ),
      );
    }, timeoutMs);
    const onData = (chunk: Buffer) => {
      buffer += chunk.toString("utf8");
      const newline = buffer.indexOf("\n");
      if (newline === -1) return;
      cleanup();
      resolve(buffer.slice(0, newline).trim());
    };
    const onExit = (code: number | null) => {
      cleanup();
      reject(new Error(`oci-e2e-registry.py exited ${code} before announcing`));
    };
    const cleanup = () => {
      clearTimeout(timer);
      stdout.off("data", onData);
      child.off("exit", onExit);
    };
    stdout.on("data", onData);
    child.once("exit", onExit);
  });
}

async function terminate(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode) return;
  child.kill("SIGTERM");
  const deadline = Date.now() + 2000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null || child.signalCode) return;
    await delay(50);
  }
  if (child.exitCode === null && !child.signalCode) {
    child.kill("SIGKILL");
  }
}

/**
 * Spawn `scripts/oci-e2e-registry.py` on the fixed E2E port.
 * First stdout line is JSON `{reference, ready, alias, architecture}`.
 */
export async function startLocalOciRegistry(
  port = REGISTRY_PORT,
): Promise<LocalOciRegistry> {
  const script = path.join(REPO_ROOT, "scripts/oci-e2e-registry.py");
  const child = spawn("python3", [script, "--port", String(port)], {
    cwd: REPO_ROOT,
    env: { ...process.env, FIRECRAB_OCI_E2E_PORT: String(port) },
    stdio: ["ignore", "pipe", "pipe"],
  });
  const stderr: string[] = [];
  child.stderr?.on("data", (chunk: Buffer) => {
    stderr.push(chunk.toString("utf8"));
  });
  child.on("error", (error) => {
    throw new Error(`failed to spawn oci-e2e-registry.py: ${error.message}`);
  });

  let announcement: RegistryAnnouncement;
  try {
    const line = await firstStdoutLine(child, 10_000);
    announcement = JSON.parse(line) as RegistryAnnouncement;
  } catch (error) {
    await terminate(child);
    const detail = stderr.join("").trim();
    throw new Error(
      `${error instanceof Error ? error.message : String(error)}${detail ? `\n${detail}` : ""}`,
    );
  }

  if (
    !announcement.reference ||
    !announcement.alias ||
    !announcement.ready ||
    !announcement.architecture
  ) {
    await terminate(child);
    throw new Error(`registry announcement missing fields: ${JSON.stringify(announcement)}`);
  }

  return {
    announcement,
    async stop() {
      await terminate(child);
    },
  };
}
