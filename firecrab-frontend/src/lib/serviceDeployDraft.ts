const KEY = "firecrab.service-deploy-draft";
export interface ServiceDeployDraft {
  serviceId: string;
  image: string;
  port: string;
  network: string;
}
export function readServiceDeployDraft(): ServiceDeployDraft | null {
  try {
    const value = JSON.parse(sessionStorage.getItem(KEY) ?? "null");
    return value &&
      ["serviceId", "image", "port", "network"].every(
        (key) => typeof value[key] === "string",
      )
      ? value
      : null;
  } catch {
    return null;
  }
}
export function saveServiceDeployDraft(value: ServiceDeployDraft) {
  try {
    sessionStorage.setItem(KEY, JSON.stringify(value));
  } catch {
    /* The live form remains usable if browser storage is unavailable. */
  }
}
export function clearServiceDeployDraft() {
  try {
    sessionStorage.removeItem(KEY);
  } catch {
    /* Browser storage can be disabled. */
  }
}
