import AgentManager from "./AgentManager";
import AgentEditor from "./AgentEditor";
import type { VmResponse } from "../bindings";
import "../styles/ai-sandbox.css";

export default function AiSandbox(props: {
  vms: VmResponse[];
  onUnsavedChange: (dirty: boolean) => void;
}) {
  const params = new URLSearchParams(window.location.hash.split("?")[1] || "");
  const editor = params.get("editor");
  if (editor)
    return (
      <AgentEditor
        vmId={params.get("vm") || ""}
        agentId={editor}
        controllerId={params.get("controller") || ""}
      />
    );
  return <AgentManager {...props} />;
}
