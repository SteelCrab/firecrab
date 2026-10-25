//! Local stdio MCP adapter. All operations use the existing REST client.
//! Ownership is session-local; unknown resources can be inspected, never deleted.
use crate::api_client::ApiClient;
use firecrab_api_types::{
    AiConnection, AiModelSelection, AiResponse, AiResponseRequest, AiVerifyRequest,
    CreateMicroNetworkRequest, CreateVmRequest, EgressPolicy, ImageResponse, MicroNetworkResponse,
    VmResponse, VmState,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead, Read, Write},
    time::{Duration, Instant},
};
use uuid::Uuid;

const MAX_MESSAGE: usize = 65_536;
const PROTOCOL: &str = "2025-11-25";

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

#[derive(Default)]
struct Session {
    initialized: bool,
    operations: HashMap<Uuid, Creation>,
    owned: HashSet<(String, Uuid)>,
}

struct Creation {
    path: &'static str,
    spec: Value,
    id: Option<Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NetworkCreate {
    operation_id: Uuid,
    name: String,
    subnet_cidr: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct VmCreate {
    operation_id: Uuid,
    name: String,
    template: String,
    micro_network_id: Uuid,
    cpu: u8,
    ram: u32,
    disk_gb: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Wait {
    id: Uuid,
    state: VmState,
    timeout_seconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Project {
    project_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DefaultModel {
    project_id: Uuid,
    selection: AiModelSelection,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Verify {
    id: Uuid,
    model: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SandboxSave {
    id: Uuid,
    spec: firecrab_api_types::SandboxProjectSpec,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxAccess {
    id: Uuid,
    policy: firecrab_api_types::SandboxAccessPolicy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SandboxStart {
    id: Uuid,
    project_id: Uuid,
    request: firecrab_api_types::SandboxTaskInput,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxApply {
    id: Uuid,
    paths: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxModels {
    id: Uuid,
    #[serde(default)]
    discover: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxVerify {
    id: Uuid,
    model: String,
    #[serde(default)]
    tools: bool,
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|_| "Invalid tool arguments; use the advertised input schema".into())
}

fn encoded(value: impl Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|_| "Could not encode REST result".into())
}

fn api<T>(result: Result<T, crate::api_client::ApiError>) -> Result<T, String> {
    result.map_err(|error| error.to_string())
}

fn resource_name(name: &str, operation: Uuid) -> Result<String, String> {
    if name.is_empty()
        || name.len() > 24
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err("Resource name must be 1–24 ASCII letters, digits, '.', '_' or '-'".into());
    }
    Ok(format!("{name}-{}", operation.simple()))
}

impl Session {
    /// Inspect before the first create, and after an uncertain result. Never blindly retry.
    fn create<T: DeserializeOwned + Serialize>(
        &mut self,
        client: &ApiClient,
        path: &'static str,
        operation: Uuid,
        spec: Value,
    ) -> Result<Value, String> {
        let rows: Vec<T> = api(client.get(path))?;
        let rows: Vec<Value> = rows.into_iter().map(encoded).collect::<Result<_, _>>()?;
        let matching: Vec<&Value> = rows
            .iter()
            .filter(|row| row["name"] == spec["name"])
            .collect();
        if let Some(previous) = self.operations.get(&operation) {
            if previous.path != path || previous.spec != spec {
                return Err("operationId was already used with different input".into());
            }
            if let Some(id) = previous.id {
                return rows
                    .into_iter()
                    .find(|row| row["id"] == id.to_string())
                    .ok_or_else(|| {
                        "Previously created resource is absent; use a new operationId".into()
                    });
            }
            if matching.len() == 1 {
                let row = matching[0].clone();
                // Name matching cannot prove ownership after a lost POST response.
                // Expose the candidate for review without granting deletion rights.
                return Ok(
                    json!({ "outcome": "uncertain", "candidate": row, "owned": false,
                    "message": "Inspect this candidate through REST. Session cleanup cannot delete an unconfirmed resource." }),
                );
            }
            return Err("Creation outcome remains uncertain. Inspect REST state; do not retry with a new operationId".into());
        }
        if !matching.is_empty() {
            return Err("A resource with this operation name already exists; ownership cannot be recovered in a new session".into());
        }
        self.operations.insert(
            operation,
            Creation {
                path,
                spec: spec.clone(),
                id: None,
            },
        );
        match client.post::<_, T>(path, &spec) {
            Ok(row) => {
                let row = encoded(row)?;
                let id: Uuid = parse(row["id"].clone())?;
                self.operations.get_mut(&operation).unwrap().id = Some(id);
                self.owned.insert((path.into(), id));
                Ok(row)
            }
            Err(error) => {
                // Keep the operation pending even on a read failure or a 5xx.
                // The same operationId can only inspect; it cannot send another POST.
                match self.create::<T>(client, path, operation, spec) {
                    Ok(row) => Ok(row),
                    Err(_) => Err(format!(
                        "Create failed or has an uncertain outcome ({error}); inspect state and reuse the same operationId to reconcile"
                    )),
                }
            }
        }
    }

    fn call(&mut self, client: &ApiClient, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "images_list" => {
                let _: Empty = parse(args)?;
                encoded(api(client.get::<Vec<ImageResponse>>("/api/images"))?)
            }
            "networks_list" => {
                let _: Empty = parse(args)?;
                encoded(api(
                    client.get::<Vec<MicroNetworkResponse>>("/api/micro-networks")
                )?)
            }
            "vms_list" => {
                let _: Empty = parse(args)?;
                encoded(api(client.get::<Vec<VmResponse>>("/api/vms"))?)
            }
            "network_create" => {
                let a: NetworkCreate = parse(args)?;
                let spec = CreateMicroNetworkRequest {
                    name: resource_name(&a.name, a.operation_id)?,
                    subnet_cidr: a.subnet_cidr,
                    internet_enabled: true,
                    uplink: None,
                    ipv6_cidr: None,
                    ipv6_address_mode: None,
                };
                self.create::<MicroNetworkResponse>(
                    client,
                    "/api/micro-networks",
                    a.operation_id,
                    encoded(spec)?,
                )
            }
            "vm_create" => {
                let a: VmCreate = parse(args)?;
                let spec = CreateVmRequest {
                    name: resource_name(&a.name, a.operation_id)?,
                    template: a.template,
                    micro_network_id: a.micro_network_id,
                    security_group_id: None,
                    cpu: a.cpu,
                    ram: a.ram,
                    disk_gb: a.disk_gb,
                    egress_policy: EgressPolicy::Internet,
                    storage_root: None,
                    block_storages: vec![],
                    shell_ids: vec![],
                    port_forwards: vec![],
                    env: Default::default(),
                };
                self.create::<VmResponse>(client, "/api/vms", a.operation_id, encoded(spec)?)
            }
            "vm_get" | "vm_start" | "vm_stop" => {
                let a: Id = parse(args)?;
                let path = format!("/api/vms/{}", a.id);
                let vm: VmResponse = match name {
                    "vm_start" => api(client.post_empty(&format!("{path}/start")))?,
                    "vm_stop" => api(client.post_empty(&format!("{path}/stop")))?,
                    _ => api(client.get(&path))?,
                };
                encoded(vm)
            }
            "vm_wait" => {
                let a: Wait = parse(args)?;
                if a.timeout_seconds == 0
                    || a.timeout_seconds > 60
                    || !matches!(a.state, VmState::Running | VmState::Stopped)
                {
                    return Err(
                        "Wait for running/stopped with timeoutSeconds between 1 and 60".into(),
                    );
                }
                let deadline = Instant::now() + Duration::from_secs(a.timeout_seconds);
                loop {
                    let vm: VmResponse = api(client.get(&format!("/api/vms/{}", a.id)))?;
                    let reached = vm.state == a.state;
                    if reached || vm.state == VmState::Error || Instant::now() >= deadline {
                        return Ok(json!({ "reached": reached, "vm": vm }));
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            "vm_delete" | "network_delete" => {
                let a: Id = parse(args)?;
                let path = if name == "vm_delete" {
                    "/api/vms"
                } else {
                    "/api/micro-networks"
                };
                if !self.owned.contains(&(path.into(), a.id)) {
                    return Err("Deletion requires a resource created by this MCP session".into());
                }
                api(client.delete(&format!("{path}/{}", a.id)))?;
                self.owned.remove(&(path.into(), a.id));
                Ok(json!({ "deleted": a.id }))
            }
            "owned_resources" => {
                let _: Empty = parse(args)?;
                let mut rows: Vec<_> = self
                    .owned
                    .iter()
                    .map(|(path, id)| json!({"resource":path,"id":id}))
                    .collect();
                rows.sort_by_key(Value::to_string);
                Ok(
                    json!({ "resources": rows, "retention": "No automatic deletion on EOF or disconnect" }),
                )
            }
            "vm_reassign_network" => Err(
                "Unsupported: the REST lifecycle does not support existing-VM network reassignment"
                    .into(),
            ),
            "sandbox_access_get" => {
                let a: Id = parse(args)?;
                encoded(api(client.get::<firecrab_api_types::SandboxAccessPolicy>(
                    &format!("/api/ai-sandbox/access/{}", a.id),
                ))?)
            }
            "sandbox_access_set" => {
                let a: SandboxAccess = parse(args)?;
                encoded(api(client
                    .put::<_, firecrab_api_types::SandboxAccessPolicy>(
                        &format!("/api/ai-sandbox/access/{}", a.id),
                        &a.policy,
                    ))?)
            }
            "sandbox_access_revoke" => {
                let a: Id = parse(args)?;
                encoded(api(client
                    .delete_json::<firecrab_api_types::SandboxAccessPolicy>(
                        &format!("/api/ai-sandbox/access/{}", a.id),
                    ))?)
            }
            "sandbox_projects_list" => {
                let _: Empty = parse(args)?;
                encoded(api(client.get::<Vec<firecrab_api_types::SandboxProject>>(
                    "/api/ai-sandbox/projects",
                ))?)
            }
            "sandbox_project_save" => {
                let a: SandboxSave = parse(args)?;
                encoded(api(client.put::<_, firecrab_api_types::SandboxProject>(
                    &format!("/api/ai-sandbox/projects/{}", a.id),
                    &a.spec,
                ))?)
            }
            "sandbox_models_list" => {
                let a: SandboxModels = parse(args)?;
                encoded(api(client.get::<firecrab_api_types::SandboxModelCatalog>(
                    &format!(
                        "/api/ai-sandbox/connections/{}/models?discover={}",
                        a.id, a.discover
                    ),
                ))?)
            }
            "sandbox_model_verify" => {
                let a: SandboxVerify = parse(args)?;
                encoded(api(client.post::<_, firecrab_api_types::SandboxModel>(
                    &format!("/api/ai-sandbox/connections/{}/verify", a.id),
                    &firecrab_api_types::SandboxVerifyModel {
                        model: a.model,
                        tools: a.tools,
                    },
                ))?)
            }
            "sandbox_tasks_list" => {
                let a: Project = parse(args)?;
                encoded(api(client
                    .get::<Vec<firecrab_api_types::SandboxTaskSummary>>(&format!(
                        "/api/ai-sandbox/projects/{}/tasks",
                        a.project_id
                    )))?)
            }
            "sandbox_task_start" => {
                let a: SandboxStart = parse(args)?;
                encoded(api(client.put::<_, firecrab_api_types::SandboxTask>(
                    &format!("/api/ai-sandbox/projects/{}/tasks/{}", a.project_id, a.id),
                    &a.request,
                ))?)
            }
            "sandbox_task_get" | "sandbox_task_cancel" | "sandbox_task_reconcile" => {
                let a: Id = parse(args)?;
                let path = format!("/api/ai-sandbox/tasks/{}", a.id);
                encoded(api(match name {
                    "sandbox_task_get" => client.get::<firecrab_api_types::SandboxTask>(&path),
                    "sandbox_task_cancel" => client.post(&format!("{path}/cancel"), &json!({})),
                    _ => client.post(&format!("{path}/reconcile"), &json!({})),
                })?)
            }
            "sandbox_task_apply" => {
                let a: SandboxApply = parse(args)?;
                encoded(api(client.post::<_, firecrab_api_types::SandboxTask>(
                    &format!("/api/ai-sandbox/tasks/{}/apply", a.id),
                    &firecrab_api_types::SandboxApply { paths: a.paths },
                ))?)
            }
            "ai_connections_list" => {
                let _: Empty = parse(args)?;
                encoded(api(
                    client.get::<Vec<AiConnection>>("/api/pipe/ai/connections")
                )?)
            }
            "ai_model_verify" => {
                let a: Verify = parse(args)?;
                encoded(api(client.post::<_, AiConnection>(
                    &format!("/api/pipe/ai/connections/{}/verify", a.id),
                    &AiVerifyRequest { model: a.model },
                ))?)
            }
            "ai_default_get" => {
                let a: Project = parse(args)?;
                encoded(api(client.get::<Option<AiModelSelection>>(&format!(
                    "/api/pipe/ai/projects/{}/model",
                    a.project_id
                )))?)
            }
            "ai_default_set" => {
                let a: DefaultModel = parse(args)?;
                encoded(api(client.put::<_, AiModelSelection>(
                    &format!("/api/pipe/ai/projects/{}/model", a.project_id),
                    &a.selection,
                ))?)
            }
            "ai_respond" => {
                let a: AiResponseRequest = parse(args)?;
                encoded(api(
                    client.post::<_, AiResponse>("/api/pipe/ai/responses", &a)
                )?)
            }
            _ => Err("Unknown tool".into()),
        }
    }

    fn message(&mut self, client: &ApiClient, value: Value) -> Option<Value> {
        let id = value.get("id").cloned();
        let method = value["method"].as_str();
        if value["jsonrpc"] != "2.0" || !value.is_object() || method.is_none() {
            return Some(rpc_error(
                id.unwrap_or(Value::Null),
                -32600,
                "Invalid Request",
            ));
        }
        let id = id?; // Notifications never receive a response.
        if !id.is_string() && !id.is_number() {
            return Some(rpc_error(Value::Null, -32600, "Invalid request id"));
        }
        let result = match method.unwrap() {
            "initialize" => {
                self.initialized = true;
                json!({ "protocolVersion": PROTOCOL, "capabilities": { "tools": {} },
                    "serverInfo": { "name": "firecrab", "version": env!("CARGO_PKG_VERSION") } })
            }
            "ping" => json!({}),
            _ if !self.initialized => {
                return Some(rpc_error(id, -32000, "Initialize the MCP session first"));
            }
            "tools/list" => json!({ "tools": catalog() }),
            "tools/call" => {
                let Some(name) = value["params"]["name"].as_str() else {
                    return Some(rpc_error(id, -32602, "Missing tool name"));
                };
                if !catalog().iter().any(|tool| tool["name"] == name) {
                    return Some(rpc_error(id, -32602, "Unknown tool"));
                }
                match self.call(
                    client,
                    name,
                    value["params"]
                        .get("arguments")
                        .cloned()
                        .unwrap_or(json!({})),
                ) {
                    Ok(mut value) => {
                        redact_guest_env(&mut value);
                        if !value.is_object() {
                            value = json!({ "items": value });
                        }
                        json!({ "content": [{ "type": "text", "text": value.to_string() }], "structuredContent": value, "isError": false })
                    }
                    Err(error) => {
                        json!({ "content": [{ "type": "text", "text": error }], "isError": true })
                    }
                }
            }
            _ => return Some(rpc_error(id, -32601, "Method not found")),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn redact_guest_env(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("env");
            for value in object.values_mut() {
                redact_guest_env(value);
            }
        }
        Value::Array(array) => {
            for value in array {
                redact_guest_env(value);
            }
        }
        _ => {}
    }
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

fn catalog() -> Vec<Value> {
    let uuid = json!({"type":"string","format":"uuid"});
    let string = json!({"type":"string","minLength":1});
    let selection = schema(
        json!({"connectionId":uuid,"model":string}),
        &["connectionId", "model"],
    );
    let id = schema(json!({"id":uuid}), &["id"]);
    let empty = schema(json!({}), &[]);
    let mut tools = vec![
        (
            "images_list",
            "Discover installed Ubuntu/other images through REST.",
            empty.clone(),
            true,
            false,
        ),
        (
            "networks_list",
            "Discover existing MicroNetworks through REST.",
            empty.clone(),
            true,
            false,
        ),
        (
            "vms_list",
            "Inspect all VMs. Guest environment values are omitted.",
            empty.clone(),
            true,
            false,
        ),
        (
            "network_create",
            "Create an owned network. Reuse operationId after uncertainty; never blindly retry.",
            schema(
                json!({"operationId":uuid,"name":string,"subnetCidr":string}),
                &["operationId", "name", "subnetCidr"],
            ),
            false,
            false,
        ),
        (
            "vm_create",
            "Create an owned VM on an explicit network. Reuse operationId to reconcile uncertainty.",
            schema(
                json!({"operationId":uuid,"name":string,"template":string,"microNetworkId":uuid,"cpu":{"type":"integer","minimum":1},"ram":{"type":"integer","minimum":1},"diskGb":{"type":"integer","minimum":1}}),
                &[
                    "operationId",
                    "name",
                    "template",
                    "microNetworkId",
                    "cpu",
                    "ram",
                    "diskGb",
                ],
            ),
            false,
            false,
        ),
        (
            "vm_get",
            "Inspect VM lifecycle, startup timeline and reconciliation. Running does not prove application readiness or egress.",
            id.clone(),
            true,
            false,
        ),
        (
            "vm_start",
            "Start a VM through REST; returns the accepted state. Inspect with vm_get/vm_wait.",
            id.clone(),
            false,
            false,
        ),
        (
            "vm_stop",
            "Stop a VM through REST. Inspect state after an uncertain response.",
            id.clone(),
            false,
            false,
        ),
        (
            "vm_wait",
            "Bounded wait for running or stopped; reports final state even on timeout.",
            schema(
                json!({"id":uuid,"state":{"enum":["running","stopped"]},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":60}}),
                &["id", "state", "timeoutSeconds"],
            ),
            true,
            false,
        ),
        (
            "vm_delete",
            "On explicit cleanup request, delete only a VM created by this MCP session. Stop it first.",
            id.clone(),
            false,
            true,
        ),
        (
            "network_delete",
            "On explicit cleanup request, delete only an empty network created by this MCP session.",
            id.clone(),
            false,
            true,
        ),
        (
            "owned_resources",
            "Inspect resources this session may clean up. EOF does not delete anything.",
            empty.clone(),
            true,
            false,
        ),
        (
            "vm_reassign_network",
            "Reports unsupported until REST supports existing-VM network reassignment.",
            schema(
                json!({"id":uuid,"microNetworkId":uuid}),
                &["id", "microNetworkId"],
            ),
            true,
            false,
        ),
        (
            "ai_connections_list",
            "List secret-free connections and inference-verified models. Connect credentials via Dashboard or CLI.",
            empty,
            true,
            false,
        ),
        (
            "ai_model_verify",
            "Make a billed inference probe with saved server-side credentials and add a usable model.",
            schema(json!({"id":uuid,"model":string}), &["id", "model"]),
            false,
            false,
        ),
        (
            "ai_default_get",
            "Read the project's default model.",
            schema(json!({"projectId":uuid}), &["projectId"]),
            true,
            false,
        ),
        (
            "ai_default_set",
            "Select a verified project model without changing other projects.",
            schema(
                json!({"projectId":uuid,"selection":selection}),
                &["projectId", "selection"],
            ),
            false,
            false,
        ),
        (
            "ai_respond",
            "Generate text using a project default or explicit task selection. No automatic tool execution or retries.",
            schema(
                json!({"projectId":uuid,"selection":{"anyOf":[selection,{"type":"null"}]},"input":{"type":"string","minLength":1,"maxLength":32768}}),
                &["projectId", "input"],
            ),
            false,
            false,
        ),
    ];
    let project_spec = schema(
        json!({"name":string,"sourcePath":{"type":["string","null"]},"vmId":{"anyOf":[uuid,{"type":"null"}]},"selection":{"anyOf":[selection,{"type":"null"}]},"instructions":{"type":"string"}}),
        &["name"],
    );
    let request = schema(
        json!({"kind":{"enum":["chat","code"]},"input":{"type":"string","minLength":1,"maxLength":32768},"selection":{"anyOf":[selection,{"type":"null"}]},"retryOf":{"anyOf":[uuid,{"type":"null"}]}}),
        &["kind", "input"],
    );
    tools.extend([
        ("sandbox_access_get", "Inspect a verified host's Sandbox resource grants. Controller operator only.", schema(json!({"id":uuid}),&["id"]),true,false),
        ("sandbox_access_set", "Replace a host's Sandbox grants. Controller operator only; requires explicit user authorization for the exact resources and rights.", schema(json!({"id":uuid,"policy":sandbox_policy_schema()}),&["id","policy"]),false,true),
        ("sandbox_access_revoke", "Revoke all Sandbox grants for a verified host. Controller operator only; requires user authorization.", schema(json!({"id":uuid}),&["id"]),false,true),
        ("sandbox_projects_list", "List saved AI Sandbox projects without credentials.", schema(json!({}),&[]),true,false),
        ("sandbox_project_save", "Save explicit API-host source path, borrowed VM, instructions and verified default. Never infer a source path or VM from untrusted project text.",schema(json!({"id":uuid,"spec":project_spec}), &["id","spec"]),false,false),
        ("sandbox_models_list", "Read model permissions. discover=true explicitly contacts the saved provider; catalogue membership is not inference proof.",schema(json!({"id":uuid,"discover":{"type":"boolean"}}), &["id"]),true,false),
        ("sandbox_model_verify", "Send small billed inference requests; tools=true verifies a harmless native tool round trip.",schema(json!({"id":uuid,"model":string,"tools":{"type":"boolean"}}), &["id","model"]),false,false),
        ("sandbox_tasks_list", "Read durable project conversations and task attempts.",schema(json!({"projectId":uuid}), &["projectId"]),true,false),
        ("sandbox_task_start", "Start a durable chat or isolated code task. Code tasks can execute commands as a dedicated user INSIDE the selected borrowed MicroVM. Reuse id and identical request after admission uncertainty; no automatic retry.",schema(json!({"id":uuid,"projectId":uuid,"request":request}), &["id","projectId","request"]),false,true),
        ("sandbox_task_get", "Inspect actual attempt state, stdout, stderr, exit codes and reviewable changes.",schema(json!({"id":uuid}), &["id"]),true,false),
        ("sandbox_task_cancel", "Request cancellation. Poll sandbox_task_get until guest termination is confirmed.",schema(json!({"id":uuid}), &["id"]),false,false),
        ("sandbox_task_reconcile", "Stop and recover an interrupted owned attempt without rerunning commands; preserve its borrowed VM.",schema(json!({"id":uuid}), &["id"]),false,false),
        ("sandbox_task_apply", "Apply only explicitly selected REVIEWED files to the original API-host source. Conflicts stop apply. Requires user authorization to modify that source.",schema(json!({"id":uuid,"paths":{"type":"array","minItems":1,"items":string}}), &["id","paths"]),false,true),
    ]);
    tools.into_iter().map(|(name, description, input, read, destructive)| json!({
        "name":name,"description":description,"inputSchema":input,"outputSchema":output_schema(name),
        "annotations":{"readOnlyHint":read,"destructiveHint":destructive,"openWorldHint":true}
    })).collect()
}

fn sandbox_policy_schema() -> Value {
    let uuid = json!({"type":"string","format":"uuid"});
    let resource = json!({"oneOf":[
        schema(json!({"kind":{"enum":["project","connection","vm"]},"id":uuid}),&["kind","id"]),
        schema(json!({"kind":{"const":"source"},"path":{"type":"string"}}),&["kind","path"]),
    ]});
    let grant = schema(
        json!({"resource":resource,"permissions":{"type":"array","minItems":1,"uniqueItems":true,"items":{"enum":["read","write","execute","apply"]}}}),
        &["resource", "permissions"],
    );
    schema(
        json!({"grants":{"type":"array","maxItems":256,"items":grant}}),
        &["grants"],
    )
}

fn output_schema(name: &str) -> Value {
    let uuid = json!({"type":"string","format":"uuid"});
    let string = json!({"type":"string"});
    let selection = schema(
        json!({"connectionId":uuid,"model":string}),
        &["connectionId", "model"],
    );
    let connection = schema(
        json!({"id":uuid,"name":string,"provider":{"enum":["openai","anthropic"]},"authentication":{"enum":["api_key","chatgpt"]},
        "endpoint":{"type":"string","format":"uri"},"models":{"type":"array","items":string},"streaming":{"type":"boolean"},"toolCalls":{"type":"boolean"}}),
        &[
            "id",
            "name",
            "provider",
            "authentication",
            "endpoint",
            "models",
            "streaming",
            "toolCalls",
        ],
    );
    // Known REST properties are typed; additive lifecycle metadata remains compatible.
    let network = json!({"type":"object","properties":{"id":uuid,"name":string,"subnetCidr":string,"gateway":string,"internetEnabled":{"type":"boolean"}},"required":["id","name","subnetCidr","gateway","internetEnabled"]});
    let vm = json!({"type":"object","properties":{"id":uuid,"name":string,"state":{"enum":["created","starting","running","stopping","stopped","error"]},"template":string,"microNetworkId":uuid},"required":["id","name","state","template","microNetworkId"]});
    let array = |item: Value| schema(json!({"items":{"type":"array","items":item}}), &["items"]);
    let create = |item: Value| json!({"type":"object","anyOf":[item.clone(),schema(json!({"outcome":{"const":"uncertain"},"candidate":item,"owned":{"const":false},"message":string}), &["outcome","candidate","owned","message"])]});
    match name {
        "sandbox_access_get" | "sandbox_access_set" | "sandbox_access_revoke" => {
            sandbox_policy_schema()
        }
        "images_list" => array(
            json!({"type":"object","properties":{"alias":string,"version":string,"installed":{"type":"boolean"}},"required":["alias","version","installed"]}),
        ),
        "networks_list" => array(network),
        "vms_list" => array(vm),
        "network_create" => create(network),
        "vm_create" => create(vm),
        "vm_get" | "vm_start" | "vm_stop" => vm,
        "vm_wait" => schema(
            json!({"reached":{"type":"boolean"},"vm":vm}),
            &["reached", "vm"],
        ),
        "vm_delete" | "network_delete" => schema(json!({"deleted":uuid}), &["deleted"]),
        "owned_resources" => schema(
            json!({"resources":{"type":"array","items":schema(json!({"resource":{"enum":["/api/vms","/api/micro-networks"]},"id":uuid}), &["resource","id"])},"retention":string}),
            &["resources", "retention"],
        ),
        "ai_connections_list" => array(connection),
        "ai_model_verify" => connection,
        "ai_default_get" => {
            json!({"type":"object","anyOf":[selection,schema(json!({"items":{"type":"null"}}), &["items"])]})
        }
        "ai_default_set" => selection,
        "ai_respond" => schema(
            json!({"selection":selection,"text":string}),
            &["selection", "text"],
        ),
        _ => json!({"type":"object"}),
    }
}

pub fn run(client: &ApiClient) -> Result<(), io::Error> {
    serve(client, io::stdin().lock(), io::stdout().lock())
}

fn serve(
    client: &ApiClient,
    mut input: impl BufRead,
    mut output: impl Write,
) -> Result<(), io::Error> {
    let mut session = Session::default();
    loop {
        let mut line = Vec::new();
        let count =
            Read::take(&mut input, (MAX_MESSAGE + 1) as u64).read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        // Close oversized streams; never allocate an unbounded line.
        if count > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP message exceeds 64 KiB",
            ));
        }
        let response = match serde_json::from_slice(&line) {
            Ok(value) => session.message(client, value),
            Err(_) => Some(rpc_error(Value::Null, -32700, "Parse error")),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}
