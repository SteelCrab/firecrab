//! Explicit JSON schemas advertise the same typed REST fields to MCP clients.
use super::*;
/// Annotate UUID strings; Rust deserialization still performs the actual identity parse.
fn uuid() -> Value {
    json!({"type":"string","format":"uuid"})
}
/// Describe textual fields without implying a host path or executable permission.
fn string() -> Value {
    json!({"type":"string"})
}
/// Describe an explicit boolean, distinct from omitted or nullable observations.
fn boolean() -> Value {
    json!({"type":"boolean"})
}
/// Describe nonnegative counts, byte sizes and millisecond timestamps.
fn integer() -> Value {
    json!({"type":"integer","minimum":0})
}
/// Permit JSON null for a value that is unavailable or not yet observed.
fn nullable(value: Value) -> Value {
    json!({"anyOf":[value,{"type":"null"}]})
}
/// Describe a homogeneous list using the element's complete schema.
fn array(value: Value) -> Value {
    json!({"type":"array","items":value})
}
/// Match the MCP adapter's object envelope for REST responses that contain a list.
fn items(value: Value) -> Value {
    schema(json!({"items":array(value)}), &["items"])
}
/// Describe required output fields while permitting future server response extensions.
fn record(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required})
}
/// Keep VM identity paired with the admitted runtime generation.
fn runtime() -> Value {
    schema(
        json!({"vmId":uuid(),"runtimeId":uuid()}),
        &["vmId", "runtimeId"],
    )
}
/// Describe the immutable environment generation and exact role captured by a task.
fn environment_ref() -> Value {
    schema(
        json!({"id":uuid(),"generation":uuid(),"role":string()}),
        &["id", "generation", "role"],
    )
}
/// Select an existing connection and model name without accepting provider credentials.
fn selection() -> Value {
    schema(
        json!({"connectionId":uuid(),"model":string()}),
        &["connectionId", "model"],
    )
}
/// Describe optional Git ref and source-relative skill/reference paths for project context.
pub(in crate::mcp) fn context_schema() -> Value {
    schema(
        json!({"gitRef":nullable(string()),"selectedSkills":array(string()),"references":array(string())}),
        &[],
    )
}
/// Advertise dependencies, agent assignment and a retry limit from zero through three.
pub(in crate::mcp) fn coordination_schema() -> Value {
    schema(
        json!({"dependencies":array(uuid()),"assignedAgent":nullable(string()),"retryLimit":{"type":"integer","minimum":0,"maximum":3}}),
        &[],
    )
}
/// Describe task admission inputs, including explicit retries and environment-role selection.
fn request() -> Value {
    record(
        json!({"kind":{"enum":["chat","code"]},"input":string(),"selection":nullable(selection()),"retryOf":nullable(uuid()),"coordination":coordination_schema(),"environmentRole":nullable(string())}),
        &["kind", "input"],
    )
}
/// Describe saved project context, caller ownership and optional VM/environment/model defaults.
fn project() -> Value {
    record(
        json!({"id":uuid(),"owner":json!({"oneOf":[{"const":"local"},{"type":"object","properties":{"host":uuid()},"required":["host"]}]}),"spec":record(json!({"name":string(),"sourcePath":nullable(string()),"vmId":nullable(uuid()),"environmentId":nullable(uuid()),"selection":nullable(selection()),"instructions":string(),"context":context_schema()}),&["name"]),"createdAt":integer()}),
        &["id", "owner", "spec", "createdAt"],
    )
}
/// Keep tool input, actual stdout/stderr, nullable exit status and event sequence together.
fn event() -> Value {
    record(
        json!({"sequence":integer(),"at":integer(),"name":string(),"input":string(),"output":string(),"stderr":string(),"exitCode":nullable(json!({"type":"integer"}))}),
        &[
            "sequence", "at", "name", "input", "output", "stderr", "exitCode",
        ],
    )
}
/// Describe before/after file content and modes, plus separately recorded apply status.
fn change() -> Value {
    record(
        json!({"path":string(),"before":nullable(string()),"after":nullable(string()),"beforeMode":nullable(integer()),"afterMode":nullable(integer()),"applied":boolean()}),
        &["path", "before", "after", "applied"],
    )
}
/// Describe retained bytes by task/attempt identity, relative path, size and SHA-256.
fn artifact() -> Value {
    record(
        json!({"id":uuid(),"taskId":uuid(),"attemptId":uuid(),"path":string(),"size":integer(),"sha256":string()}),
        &["id", "taskId", "attemptId", "path", "size", "sha256"],
    )
}
/// Build task output fields; summaries expose counts and details expose events and artifacts.
/// A result state is separate from failed tests or a command's nonzero exit status.
fn task(summary: bool) -> Value {
    let mut fields = json!({"id":uuid(),"projectId":uuid(),"attemptId":uuid(),"runtime":nullable(runtime()),"request":request(),"selection":selection(),"state":{"enum":["waiting","accepted","in_progress","result","merged","failed","cancelled","interrupted"]},"createdAt":integer(),"updatedAt":integer(),"result":string(),"errorCode":nullable(string()),"error":nullable(string()),"retryRoot":nullable(uuid()),"retryNumber":integer(),"environment":nullable(environment_ref())});
    if summary {
        fields["eventCount"] = integer();
        fields["changeCount"] = integer();
        fields["testCount"] = integer();
        fields["failedTestCount"] = integer();
        fields["artifactCount"] = integer();
        fields["lastTool"] = nullable(string());
    } else {
        fields["events"] = array(event());
        fields["changes"] = array(change());
        fields["artifacts"] = array(artifact());
        fields["tests"] = array(record(
            json!({"name":string(),"eventSequence":integer(),"passed":boolean()}),
            &["name", "eventSequence", "passed"],
        ));
        fields["gitCommit"] = nullable(string());
        fields["dependencyResults"] = array(record(
            json!({"taskId":uuid(),"attemptId":uuid(),"assignedAgent":nullable(string()),"result":string(),"changedPaths":array(string()),"tests":array(record(json!({"name":string(),"eventSequence":integer(),"passed":boolean()}),&["name","eventSequence","passed"])),"artifacts":array(artifact())}),
            &["taskId", "attemptId", "result"],
        ));
    }
    record(
        fields,
        &[
            "id",
            "projectId",
            "attemptId",
            "request",
            "selection",
            "state",
            "createdAt",
            "updatedAt",
            "result",
        ],
    )
}
/// Describe a discovered model and the independent model/tool verification observations.
fn model() -> Value {
    record(
        json!({"id":string(),"name":string(),"verified":boolean(),"toolsVerified":boolean()}),
        &["id", "name", "verified", "toolsVerified"],
    )
}
/// Describe version-one immutable roles, template hashes, execution modes, allows and TTL.
/// Server validation additionally checks ownership, dependency ordering and mode policy.
pub(super) fn environment_spec() -> Value {
    let pin = schema(
        json!({"name":string(),"version":string(),"kernelSha256":string(),"rootfsSha256":string(),"initrdSha256":nullable(string()),"bootArgsSha256":string()}),
        &[
            "name",
            "version",
            "kernelSha256",
            "rootfsSha256",
            "bootArgsSha256",
        ],
    );
    let mode = json!({"oneOf":[schema(json!({"kind":{"const":"agent_sandbox"},"runtime":{"enum":["docker","nspawn"]},"image":string()}),&["kind","runtime","image"]),schema(json!({"kind":{"const":"persistent_workspace"}}),&["kind"])]});
    let service = schema(
        json!({"name":string(),"port":{"type":"integer","minimum":1,"maximum":65535},"readiness":boolean()}),
        &["name", "port"],
    );
    let role = schema(
        json!({"name":string(),"template":pin,"cpu":{"type":"integer","minimum":1,"maximum":255},"memoryMib":{"type":"integer","minimum":1},"diskGb":{"type":"integer","minimum":1,"maximum":65535},"dependsOn":array(string()),"services":array(service),"executionMode":mode}),
        &[
            "name",
            "template",
            "cpu",
            "memoryMib",
            "diskGb",
            "executionMode",
        ],
    );
    schema(
        json!({"version":{"const":1},"subnetCidr":string(),"roles":array(role),"allow":array(schema(json!({"from":string(),"to":string(),"port":{"type":"integer","minimum":1,"maximum":65535}}),&["from","to","port"])),"retention":schema(json!({"ttlSeconds":nullable(integer())}),&[])}),
        &["version", "subnetCidr", "roles"],
    )
}
/// Describe the saved environment and separate agent, network and application readiness.
fn environment() -> Value {
    let readiness = json!({"enum":["pending","ready","unsupported","failed"]});
    let service = record(
        json!({"name":string(),"address":string(),"port":integer(),"ready":boolean()}),
        &["name", "address", "port", "ready"],
    );
    let role = record(
        json!({"role":string(),"vmId":uuid(),"securityGroupId":uuid(),"runtime":nullable(runtime()),"ipv4":nullable(string()),"agent":readiness,"network":readiness,"application":readiness,"services":array(service),"error":nullable(string())}),
        &[
            "role",
            "vmId",
            "securityGroupId",
            "agent",
            "network",
            "application",
            "services",
        ],
    );
    record(
        json!({"id":uuid(),"projectId":uuid(),"generation":uuid(),"spec":environment_spec(),"networkId":uuid(),"roles":array(role),"state":{"enum":["provisioning","ready","stopped","cleanup_pending","deleted","failed"]},"createdAt":integer(),"updatedAt":integer(),"expiresAt":nullable(integer()),"exportedAt":nullable(integer()),"error":nullable(string())}),
        &[
            "id",
            "projectId",
            "generation",
            "spec",
            "networkId",
            "roles",
            "state",
            "createdAt",
            "updatedAt",
        ],
    )
}
/// Allow only explicit task-runtime ports, declared environment services or saved Nginx routes.
pub(super) fn tunnel_spec() -> Value {
    let target = json!({"oneOf":[schema(json!({"kind":{"const":"vm_app"},"runtime":runtime(),"port":{"type":"integer","minimum":1,"maximum":65535}}),&["kind","runtime","port"]),schema(json!({"kind":{"const":"nginx"},"host":string()}),&["kind","host"]),schema(json!({"kind":{"const":"environment_service"},"environment":environment_ref(),"service":string()}),&["kind","environment","service"])]});
    schema(
        json!({"taskId":uuid(),"subdomain":string(),"target":target}),
        &["taskId", "subdomain", "target"],
    )
}
/// Keep helper connectivity separate from public HTTPS, WebSocket and retirement evidence.
fn tunnel() -> Value {
    let verification = json!({"enum":["unverified","verified"]});
    record(
        json!({"id":uuid(),"projectId":uuid(),"taskId":uuid(),"attemptId":uuid(),"runtime":runtime(),"spec":tunnel_spec(),"state":{"enum":["waiting","connecting","connected","stopping","stopped","deleting","deleted","superseded","needs_reconciliation"]},"appReady":boolean(),"connectorId":nullable(uuid()),"publicUrl":nullable(string()),"helperConnected":boolean(),"publicHttps":verification,"webSocketHandshake":verification,"publicShutdown":verification,"remoteRetirement":{"enum":["not_requested","operator_required"]},"localRemoved":boolean(),"createdAt":integer(),"updatedAt":integer(),"errorCode":nullable(string())}),
        &[
            "id",
            "projectId",
            "taskId",
            "attemptId",
            "runtime",
            "spec",
            "state",
            "appReady",
            "helperConnected",
            "publicHttps",
            "webSocketHandshake",
            "publicShutdown",
            "remoteRetirement",
            "localRemoved",
        ],
    )
}
/// Require PID, start ticks and guest boot ID together to prevent signaling a reused PID.
pub(super) fn process() -> Value {
    schema(
        json!({"pid":{"type":"integer","minimum":1},"startTicks":integer(),"bootId":string(),"state":string()}),
        &["pid", "startTicks", "bootId", "state"],
    )
}
/// Choose the advertised result envelope for each Sandbox tool, including bounded waits.
/// Successful signal submission deliberately reports terminationConfirmed as false.
pub(in crate::mcp) fn output_schema(name: &str) -> Value {
    match name {
        "sandbox_artifact_read" => schema(
            json!({"artifact":artifact(),"encoding":{"const":"base64"},"content":{"type":"string","contentEncoding":"base64"}}),
            &["artifact", "encoding", "content"],
        ),
        "sandbox_environment_images" => items(record(
            json!({"name":string(),"version":string(),"kernelSha256":string(),"rootfsSha256":string(),"initrdSha256":nullable(string()),"bootArgsSha256":string()}),
            &[
                "name",
                "version",
                "kernelSha256",
                "rootfsSha256",
                "bootArgsSha256",
            ],
        )),
        "sandbox_projects_list" => items(project()),
        "sandbox_project_save" | "sandbox_project_get" => project(),
        "sandbox_project_delete" => schema(
            json!({"id":uuid(),"deleteAccepted":{"const":true},"confirmedAbsent":boolean()}),
            &["id", "deleteAccepted", "confirmedAbsent"],
        ),
        "sandbox_models_list" => record(
            json!({"models":array(model()),"discoveryAvailable":boolean()}),
            &["models", "discoveryAvailable"],
        ),
        "sandbox_model_verify" => model(),
        "sandbox_tasks_list" => items(task(true)),
        "sandbox_task_start"
        | "sandbox_task_get"
        | "sandbox_task_cancel"
        | "sandbox_task_reconcile"
        | "sandbox_task_apply" => task(false),
        "sandbox_task_wait" => schema(
            json!({"reached":boolean(),"task":task(false)}),
            &["reached", "task"],
        ),
        "sandbox_environments_list" => items(environment()),
        "sandbox_environment_get"
        | "sandbox_environment_save"
        | "sandbox_environment_reconcile"
        | "sandbox_environment_stop"
        | "sandbox_environment_resume"
        | "sandbox_environment_delete" => environment(),
        "sandbox_environment_export" => record(
            json!({"environment":environment_ref(),"taskIds":array(uuid()),"exportedAt":integer()}),
            &["environment", "taskIds", "exportedAt"],
        ),
        "sandbox_tunnel_addresses" => items(record(
            json!({"subdomain":string(),"publicUrl":string(),"connected":boolean(),"claimedBy":nullable(uuid())}),
            &["subdomain", "publicUrl", "connected", "claimedBy"],
        )),
        "sandbox_tunnels_list" | "sandbox_tunnels_inventory" => items(tunnel()),
        "sandbox_tunnel_retarget"
        | "sandbox_tunnel_save"
        | "sandbox_tunnel_get"
        | "sandbox_tunnel_publish"
        | "sandbox_tunnel_reconcile"
        | "sandbox_tunnel_stop"
        | "sandbox_tunnel_delete"
        | "sandbox_tunnel_verify" => tunnel(),
        "sandbox_guest_health" => record(
            json!({"version":integer(),"init":string(),"execution":boolean(),"docker":boolean(),"nspawn":boolean(),"unsupported":nullable(string())}),
            &[
                "version",
                "init",
                "execution",
                "docker",
                "nspawn",
                "unsupported",
            ],
        ),
        "sandbox_guest_processes" => items(process()),
        "sandbox_guest_signal" => schema(
            json!({"sent":{"const":true},"terminationConfirmed":{"const":false}}),
            &["sent", "terminationConfirmed"],
        ),
        _ => json!({"type":"object"}),
    }
}
