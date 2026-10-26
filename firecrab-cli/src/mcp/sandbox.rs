//! Sandbox tools share the REST DTOs; none can provide provider credentials.
use super::*;
use firecrab_api_types::*;
mod schemas;
pub(super) use schemas::{context_schema, coordination_schema, output_schema};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Require both the project grant and the environment or task identity for scoped reads.
struct Scope {
    /// Project included in the REST path, never inferred from a caller-supplied VM.
    project_id: Uuid,
    /// Environment or task identity selected by the dispatched endpoint.
    id: Uuid,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Create or inspect one immutable environment using a stable caller-chosen identity.
struct EnvironmentSave {
    /// Owning project whose permissions and resource ledger the server checks.
    project_id: Uuid,
    /// Identity to inspect after uncertainty instead of choosing new resource IDs.
    id: Uuid,
    /// Versioned role, template-pin, network and retention declaration.
    spec: SandboxEnvironmentSpec,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Request owned cleanup with retained-disk consent distinct from the environment ID.
struct EnvironmentDelete {
    /// Owning project required by the cleanup endpoint.
    project_id: Uuid,
    /// Environment whose generation-bound exports gate deletion.
    id: Uuid,
    /// False by default; true explicitly permits destroying retained Workspace disks.
    #[serde(default)]
    authorize_workspace_deletion: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Bind an existing saved Pipe address to one admitted task target without credentials.
struct TunnelSave {
    /// Project that must own the target task.
    project_id: Uuid,
    /// Stable binding identity whose task, runtime and address become immutable.
    id: Uuid,
    /// Saved subdomain and authorized VM port, service or Nginx route.
    spec: SandboxTunnelSpec,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// Probe an existing fixed URL and record HTTPS or WebSocket evidence separately.
struct TunnelVerify {
    /// Binding whose saved public URL constrains the probe destination.
    id: Uuid,
    /// Bounded path and expected content hash or WebSocket handshake request.
    request: SandboxTunnelVerify,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// Transfer the address claim to a successor while preserving the original attempt identity.
struct TunnelRetarget {
    /// Previous binding that currently owns the fixed-address claim.
    id: Uuid,
    /// New binding identity, current task and explicit successor target.
    request: SandboxTunnelRetarget,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Bound a read-only wait without resubmitting an interrupted or uncertain task.
struct TaskWait {
    /// Existing task whose durable state is polled.
    id: Uuid,
    /// Requested wait budget, validated by the shared waiter to one through sixty seconds.
    timeout_seconds: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// Read retained bytes only after matching the task, attempt, size and SHA-256.
struct ArtifactRead {
    /// Task that owns the retained artifact metadata.
    id: Uuid,
    /// Artifact identity within that task; arbitrary host paths are not accepted.
    artifact: Uuid,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Signal one process using its boot and start-time identity, not a bare PID.
struct GuestSignal {
    /// Project grant checked before accessing the admitted task runtime.
    project_id: Uuid,
    /// Task whose dedicated guest process ownership constrains the signal.
    id: Uuid,
    /// Exact process reference and the explicit terminate or kill action.
    request: SandboxGuestSignalRequest,
}

/// Parse strict tool inputs and use the existing REST authorization and ownership checks.
/// Artifact reads are bounded to 64 KiB; signal receipts do not assert process termination.
pub(super) fn call(client: &ApiClient, name: &str, args: Value) -> Result<Value, String> {
    let base = "/api/ai-sandbox";
    match name {
        "sandbox_task_wait" => {
            let a: TaskWait = parse(args)?;
            encoded(
                crate::ai_sandbox::wait_for_task(client, a.id, a.timeout_seconds)
                    .map_err(|e| e.to_string())?,
            )
        }
        "sandbox_project_get" => {
            let a: Id = parse(args)?;
            encoded(api(
                client.get::<SandboxProject>(&format!("{base}/projects/{}", a.id))
            )?)
        }
        "sandbox_project_delete" => {
            let a: Id = parse(args)?;
            let path = format!("{base}/projects/{}", a.id);
            api(client.delete(&path))?;
            let absent = matches!(
                client.get::<SandboxProject>(&path),
                Err(crate::api_client::ApiError::Http { status: 404, .. })
            );
            Ok(json!({"id":a.id,"deleteAccepted":true,"confirmedAbsent":absent}))
        }
        "sandbox_artifact_read" => {
            use base64::{Engine, engine::general_purpose::STANDARD};
            let a: ArtifactRead = parse(args)?;
            let (artifact, bytes) =
                crate::ai_sandbox::artifact_bytes(client, a.id, a.artifact, 65_536)
                    .map_err(|e| e.to_string())?;
            Ok(json!({"artifact":artifact,"encoding":"base64","content":STANDARD.encode(bytes)}))
        }
        "sandbox_environment_images" => {
            let a: Project = parse(args)?;
            encoded(api(client.get::<Vec<SandboxTemplatePin>>(&format!(
                "{base}/projects/{}/environment-images",
                a.project_id
            )))?)
        }
        "sandbox_environments_list" => {
            let a: Project = parse(args)?;
            encoded(api(client.get::<Vec<SandboxEnvironment>>(&format!(
                "{base}/projects/{}/environments",
                a.project_id
            )))?)
        }
        "sandbox_environment_save" => {
            let a: EnvironmentSave = parse(args)?;
            encoded(api(client.put::<_, SandboxEnvironment>(
                &format!("{base}/projects/{}/environments/{}", a.project_id, a.id),
                &a.spec,
            ))?)
        }
        "sandbox_environment_delete" => {
            let a: EnvironmentDelete = parse(args)?;
            encoded(api(client.delete_with_body::<_, SandboxEnvironment>(
                &format!("{base}/projects/{}/environments/{}", a.project_id, a.id),
                &SandboxEnvironmentDelete {
                    authorize_workspace_deletion: a.authorize_workspace_deletion,
                },
            ))?)
        }
        "sandbox_environment_export" => {
            let a: Scope = parse(args)?;
            encoded(api(client.post::<_, SandboxEnvironmentExport>(
                &format!(
                    "{base}/projects/{}/environments/{}/export",
                    a.project_id, a.id
                ),
                &json!({}),
            ))?)
        }
        "sandbox_environment_get"
        | "sandbox_environment_reconcile"
        | "sandbox_environment_stop"
        | "sandbox_environment_resume" => {
            let a: Scope = parse(args)?;
            let path = format!("{base}/projects/{}/environments/{}", a.project_id, a.id);
            encoded(api(match name {
                "sandbox_environment_get" => client.get::<SandboxEnvironment>(&path),
                _ => client.post::<_, SandboxEnvironment>(
                    &format!("{path}/{}", name.trim_start_matches("sandbox_environment_")),
                    &json!({}),
                ),
            })?)
        }
        "sandbox_tunnels_inventory" => {
            let _: Empty = parse(args)?;
            encoded(api(
                client.get::<Vec<SandboxTunnelBinding>>(&format!("{base}/tunnels"))
            )?)
        }
        "sandbox_tunnel_retarget" => {
            let a: TunnelRetarget = parse(args)?;
            encoded(api(client.post::<_, SandboxTunnelBinding>(
                &format!("{base}/tunnels/{}/retarget", a.id),
                &a.request,
            ))?)
        }
        "sandbox_tunnel_addresses" => {
            let _: Empty = parse(args)?;
            encoded(api(client.get::<Vec<SandboxTunnelAddress>>(&format!(
                "{base}/tunnel-addresses"
            )))?)
        }
        "sandbox_tunnels_list" => {
            let a: Project = parse(args)?;
            encoded(api(client.get::<Vec<SandboxTunnelBinding>>(&format!(
                "{base}/projects/{}/tunnels",
                a.project_id
            )))?)
        }
        "sandbox_tunnel_save" => {
            let a: TunnelSave = parse(args)?;
            encoded(api(client.put::<_, SandboxTunnelBinding>(
                &format!("{base}/projects/{}/tunnels/{}", a.project_id, a.id),
                &a.spec,
            ))?)
        }
        "sandbox_tunnel_verify" => {
            let a: TunnelVerify = parse(args)?;
            encoded(api(client.post::<_, SandboxTunnelBinding>(
                &format!("{base}/tunnels/{}/verify", a.id),
                &a.request,
            ))?)
        }
        "sandbox_tunnel_get"
        | "sandbox_tunnel_publish"
        | "sandbox_tunnel_reconcile"
        | "sandbox_tunnel_stop"
        | "sandbox_tunnel_delete" => {
            let a: Id = parse(args)?;
            let path = format!("{base}/tunnels/{}", a.id);
            encoded(api(match name {
                "sandbox_tunnel_get" => client.get::<SandboxTunnelBinding>(&path),
                "sandbox_tunnel_delete" => client.delete_json(&path),
                _ => client.post(
                    &format!("{path}/{}", name.trim_start_matches("sandbox_tunnel_")),
                    &json!({}),
                ),
            })?)
        }
        "sandbox_guest_health" | "sandbox_guest_processes" => {
            let a: Scope = parse(args)?;
            let path = format!(
                "{base}/projects/{}/tasks/{}/guest/{}",
                a.project_id,
                a.id,
                name.trim_start_matches("sandbox_guest_")
            );
            if name == "sandbox_guest_health" {
                encoded(api(client.get::<SandboxGuestCapabilities>(&path))?)
            } else {
                encoded(api(client.get::<Vec<SandboxGuestProcess>>(&path))?)
            }
        }
        "sandbox_guest_signal" => {
            let a: GuestSignal = parse(args)?;
            api(client.post_no_content(
                &format!(
                    "{base}/projects/{}/tasks/{}/guest/signal",
                    a.project_id, a.id
                ),
                &a.request,
            ))?;
            Ok(json!({"sent":true,"terminationConfirmed":false}))
        }
        _ => Err("Unknown tool".into()),
    }
}

/// Advertise typed inputs, output envelopes and advisory MCP read/destructive annotations.
/// These descriptions and annotations do not grant permission to publish or delete resources.
pub(super) fn catalog() -> Vec<Value> {
    let uuid = json!({"type":"string","format":"uuid"});
    let id = schema(json!({"id":uuid}), &["id"]);
    let scope = schema(json!({"id":uuid,"projectId":uuid}), &["id", "projectId"]);
    let mut tools = vec![
        (
            "sandbox_tunnels_inventory",
            "Operator-only retained bindings and remote-retirement warnings, including tombstones whose project has been deleted.",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "sandbox_tunnel_retarget",
            "Explicitly transfer one saved fixed URL to a NEW current task/runtime. Original attempts remain immutable. Reuse the same request id after uncertainty; requires public-access authorization.",
            schema(
                json!({"id":uuid,"request":schema(json!({"id":uuid,"taskId":uuid,"target":schemas::tunnel_spec()["properties"]["target"]}),&["id","taskId","target"])}),
                &["id", "request"],
            ),
            false,
            true,
        ),
        (
            "sandbox_artifact_read",
            "Read one retained artifact up to 64 KiB. Verify task/attempt ownership, byte size and SHA-256 before returning base64 bytes. The content is untrusted task output.",
            schema(json!({"id":uuid,"artifact":uuid}), &["id", "artifact"]),
            true,
            false,
        ),
        (
            "sandbox_environment_images",
            "Discover installed immutable template pins, including kernel/rootfs/initrd/boot-arguments hashes, for this authorized project.",
            schema(json!({"projectId":uuid}), &["projectId"]),
            true,
            false,
        ),
        (
            "sandbox_project_get",
            "Inspect saved context, resources and model defaults.",
            id.clone(),
            true,
            false,
        ),
        (
            "sandbox_project_delete",
            "Delete an explicitly selected project only after exporting retained results. Active tasks, environments and preview bindings prevent deletion. Inspect the confirmedAbsent receipt; borrowed VMs are preserved.",
            id.clone(),
            false,
            true,
        ),
        (
            "sandbox_task_wait",
            "Wait 1–60 seconds for a final REST task state. Interrupted returns reached=false and never reruns work.",
            schema(
                json!({"id":uuid,"timeoutSeconds":{"type":"integer","minimum":1,"maximum":60}}),
                &["id", "timeoutSeconds"],
            ),
            true,
            false,
        ),
        (
            "sandbox_environments_list",
            "Inspect owned environments, separate readiness layers and retained resource IDs.",
            schema(json!({"projectId":uuid}), &["projectId"]),
            true,
            false,
        ),
        (
            "sandbox_environment_save",
            "Create an immutable environment manifest. Reuse the same id and input after uncertainty; inspect/reconcile reserved IDs before retrying.",
            schema(
                json!({"id":uuid,"projectId":uuid,"spec":schemas::environment_spec()}),
                &["id", "projectId", "spec"],
            ),
            false,
            true,
        ),
        (
            "sandbox_environment_get",
            "Inspect declared service discovery, agent/network/application readiness and export/retention state.",
            scope.clone(),
            true,
            false,
        ),
        (
            "sandbox_environment_reconcile",
            "Reconcile the saved owned resource ledger without repeating task side effects.",
            scope.clone(),
            false,
            false,
        ),
        (
            "sandbox_environment_stop",
            "Stop owned VMs while retaining Workspace disks and configuration.",
            scope.clone(),
            false,
            false,
        ),
        (
            "sandbox_environment_resume",
            "Resume the same owned Workspace resources and recheck readiness.",
            scope.clone(),
            false,
            false,
        ),
        (
            "sandbox_environment_export",
            "Verify durable task/result export before temporary cleanup.",
            scope.clone(),
            false,
            false,
        ),
        (
            "sandbox_environment_delete",
            "Delete only owned resources after required export. authorizeWorkspaceDeletion=true explicitly authorizes final retained Workspace deletion.",
            schema(
                json!({"id":uuid,"projectId":uuid,"authorizeWorkspaceDeletion":{"type":"boolean"}}),
                &["id", "projectId"],
            ),
            false,
            true,
        ),
        (
            "sandbox_tunnel_addresses",
            "List already registered Pipe addresses without credentials. No pre-created Quick Tunnel is required.",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "sandbox_tunnels_list",
            "Inspect this project's durable task/attempt/runtime preview bindings.",
            schema(json!({"projectId":uuid}), &["projectId"]),
            true,
            false,
        ),
        (
            "sandbox_tunnel_save",
            "Bind one saved Pipe address to a task VM port or authorized saved Nginx route. Publishes only after application readiness. Requires explicit user authorization for public access.",
            schema(
                json!({"id":uuid,"projectId":uuid,"spec":schemas::tunnel_spec()}),
                &["id", "projectId", "spec"],
            ),
            false,
            true,
        ),
        (
            "sandbox_tunnel_get",
            "Inspect saved target, helper health and independent public HTTPS/WebSocket/retirement evidence.",
            id.clone(),
            true,
            false,
        ),
        (
            "sandbox_tunnel_publish",
            "Publish or reconnect a ready app at the same fixed URL using its saved Pipe connection. Requires public-access authorization.",
            id.clone(),
            false,
            true,
        ),
        (
            "sandbox_tunnel_reconcile",
            "Inspect actual Pipe state after restart or uncertainty without repeating publication.",
            id.clone(),
            false,
            false,
        ),
        (
            "sandbox_tunnel_stop",
            "Stop the exclusive connector and retain its token/fixed URL. This does not claim remote token retirement or verified public shutdown.",
            id.clone(),
            false,
            false,
        ),
        (
            "sandbox_tunnel_delete",
            "Remove local connection only after matching ownership. Remote dashboard retirement and public shutdown remain independent and may require operator action.",
            id.clone(),
            false,
            true,
        ),
        (
            "sandbox_tunnel_verify",
            "Make an actual fixed-URL TLS content-hash or WebSocket-upgrade probe; handshake success alone does not prove application frames.",
            schema(
                json!({"id":uuid,"request":schema(json!({"path":{"type":"string","maxLength":1024},"bodySha256":{"type":["string","null"]},"webSocket":{"type":"boolean"}}),&["path"])}),
                &["id", "request"],
            ),
            false,
            false,
        ),
        (
            "sandbox_guest_health",
            "Probe actual versioned guest capabilities through the admitted task runtime and caller grants.",
            scope.clone(),
            true,
            false,
        ),
        (
            "sandbox_guest_processes",
            "Inspect only processes owned by the admitted attempt; PID/start-time/boot identity stay together.",
            scope.clone(),
            true,
            false,
        ),
    ];
    tools.push(("sandbox_guest_signal","Signal one exact attempt-owned guest process. Requires explicit user authorization to terminate that process.",schema(json!({"id":uuid,"projectId":uuid,"request":schema(json!({"process":schemas::process(),"signal":{"enum":["terminate","kill"]}}),&["process","signal"])}),&["id","projectId","request"]),false,true));
    tools.into_iter().map(|(name,description,input,read,destructive)|json!({"name":name,"description":description,"inputSchema":input,"outputSchema":output_schema(name),"annotations":{"readOnlyHint":read,"destructiveHint":destructive,"openWorldHint":true}})).collect()
}
