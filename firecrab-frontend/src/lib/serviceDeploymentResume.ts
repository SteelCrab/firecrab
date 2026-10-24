import { pipeCardHash, viewHash } from "../navigation";

/** Only deployment choices travel between screens; credentials stay in AUTH. */
export interface ServiceDeploymentDraft {
  serviceId: string;
  serviceName: string;
  image: string;
  microNetworkId: string;
  hostPort: number;
}

const KEY = "firecrabServiceDeployment";

export function readServiceDeploymentDraft(): ServiceDeploymentDraft | null {
  const value = window.history.state?.[KEY];
  if (!value || typeof value.serviceId !== "string" || typeof value.serviceName !== "string"
    || typeof value.image !== "string" || typeof value.microNetworkId !== "string"
    || typeof value.hostPort !== "number") return null;
  return value;
}

function saveDraft(draft: ServiceDeploymentDraft) {
  window.history.replaceState({ ...window.history.state, [KEY]: draft }, "");
}

export function clearServiceDeploymentDraft() {
  const state = { ...window.history.state };
  delete state[KEY];
  window.history.replaceState(state, "");
}

export function reconnectServiceAuth(draft: ServiceDeploymentDraft) {
  // Keeping the original entry also restores choices when the user presses Back.
  saveDraft(draft);
  window.location.hash = pipeCardHash("auth");
  saveDraft(draft);
}

export function resumeServiceDeployment(draft: ServiceDeploymentDraft) {
  clearServiceDeploymentDraft();
  window.location.hash = viewHash("services");
  saveDraft(draft);
}
