import { spawn, type ChildProcess } from "node:child_process";
import path from "node:path";
import { readFile } from "node:fs/promises";
import { setTimeout as delay } from "node:timers/promises";

import { REGISTRY_PORT, REPO_ROOT } from "./constants.js";
import { managerSshArgs, shellQuote } from "./manager.js";

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
    const onError = (error: Error) => {
      cleanup();
      reject(error);
    };
    const cleanup = () => {
      clearTimeout(timer);
      stdout.off("data", onData);
      child.off("exit", onExit);
      child.off("error", onError);
    };
    stdout.on("data", onData);
    child.once("exit", onExit);
    child.once("error", onError);
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
  const manager = managerSshArgs();
  // The API's loopback and Linux SSH runtime live inside the management VM.
  // Keep stdin open so closing the SSH session also stops the remote fixture.
  const loader = "import json,sys; exec(compile(json.loads(sys.stdin.readline()), 'oci-e2e-registry.py', 'exec'))";
  const remoteCommand = `python3 -u -c ${shellQuote(loader)} --port ${port} --exit-on-stdin-close`;
  const source = manager ? await readFile(script, "utf8") : null;
  const child = spawn(manager ? "ssh" : "python3", manager
    ? [...manager, remoteCommand]
    : [script, "--port", String(port)], {
    cwd: REPO_ROOT,
    env: { ...process.env, FIRECRAB_OCI_E2E_PORT: String(port) },
    stdio: [manager ? "pipe" : "ignore", "pipe", "pipe"],
  });
  if (source) child.stdin?.write(`${JSON.stringify(source)}\n`);
  const stderr: string[] = [];
  child.stderr?.on("data", (chunk: Buffer) => {
    stderr.push(chunk.toString("utf8"));
  });

  let announcement: RegistryAnnouncement;
  try {
    // The fixture assembles OpenSSH and its shared libraries before announcing.
    // Nested Windows/WSL hosts can need more than ten seconds for that disk I/O.
    const line = await firstStdoutLine(child, 60_000);
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
      if (manager) {
        child.stdin?.end();
        const deadline = Date.now() + 2000;
        while (child.exitCode === null && !child.signalCode && Date.now() < deadline) {
          await delay(50);
        }
      }
      await terminate(child);
    },
  };
}
