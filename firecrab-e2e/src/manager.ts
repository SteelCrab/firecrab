import path from "node:path";

/** Use the same management VM connection as the native macOS QA scripts. */
export function managerSshArgs(): string[] | null {
  const host = process.env.FIRECRAB_QA_MANAGER_HOST;
  const key = process.env.FIRECRAB_QA_MANAGER_KEY;
  if (!host && !key) return null;
  if (!host || !key) {
    throw new Error("set both FIRECRAB_QA_MANAGER_HOST and FIRECRAB_QA_MANAGER_KEY");
  }
  return [
    "-i", key,
    "-o", "BatchMode=yes",
    "-o", "ConnectTimeout=5",
    "-o", "IdentitiesOnly=yes",
    "-o", "StrictHostKeyChecking=yes",
    "-o", `UserKnownHostsFile=${path.join(path.dirname(key), "known_hosts")}`,
    `root@${host}`,
  ];
}

export function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

export function managerProxyArgs(): string[] {
  const args = managerSshArgs();
  if (!args) return [];
  const proxy = ["ssh", ...args, "-W", "[%h]:%p"].map(shellQuote).join(" ");
  return ["-o", `ProxyCommand=${proxy}`];
}
