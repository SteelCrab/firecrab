//! The browser's Host screen controls the outer VM through this native service.
//! Bind only loopback and reject foreign Host/Origin headers, including DNS rebinding.
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path as RoutePath, Query, State, ws::WebSocketUpgrade},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use firecrab_api_types::{ManagerResourceOperation, ManagerResourceStatus, ManagerResources};

use super::resource::{self, Error};
use super::skills::registry::Registry;

struct Controller {
    home: PathBuf,
    platform: resource::Platform,
    agents: Mutex<()>,
    editors: Mutex<()>,
    /// Serializes skills CLI changes and AGENTS.md writes.
    skills: Mutex<()>,
    skill_registry: Registry,
}

const ORIGINS: [&str; 4] = [
    "http://localhost:5523",
    "http://127.0.0.1:5523",
    "http://localhost:8080",
    "http://127.0.0.1:8080",
];

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = if matches!(self, Error::Busy) {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        };
        (status, Json(serde_json::json!({"error": self.to_string()}))).into_response()
    }
}

fn allowed(headers: &HeaderMap, home: &Path) -> bool {
    let Ok(instance) = super::dev::instance::load(home) else {
        return false;
    };
    let port = instance.as_ref().map_or(5524, |state| state.resource_port);
    let host = headers.get("host").and_then(|value| value.to_str().ok());
    let good_host = host.is_some_and(|host| {
        host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
    });
    let good_origin = headers.get("origin").is_none_or(|value| {
        value.to_str().is_ok_and(|origin| {
            (instance.is_none() && ORIGINS.contains(&origin))
                || instance.as_ref().is_some_and(|state| {
                    origin == format!("http://localhost:{}", state.api_port)
                        || origin == format!("http://127.0.0.1:{}", state.api_port)
                })
                || (home.join("runtime/dev-dashboard.json").is_file()
                    && super::dev::dashboard::port(home).is_ok_and(|port| {
                        origin == format!("http://localhost:{port}")
                            || origin == format!("http://127.0.0.1:{port}")
                    }))
        })
    });
    good_host && good_origin
}

async fn guard(
    State(controller): State<Arc<Controller>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if !allowed(request.headers(), &controller.home) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // WSL settings affect every distribution, so Docker instances expose no native VM mutations.
    #[cfg(not(target_os = "macos"))]
    if super::dev::instance::load(&controller.home)
        .ok()
        .flatten()
        .is_some()
    {
        return (StatusCode::NOT_IMPLEMENTED, Json(serde_json::json!({"error":"this isolated container has no native management VM resource controller"}))).into_response();
    }
    let origin = request.headers().get("origin").cloned();
    let mutation =
        request.method() == axum::http::Method::PUT || request.method() == axum::http::Method::POST;
    let id_matches = !mutation
        || resource::controller_id(&controller.home).is_ok_and(|id| {
            request
                .headers()
                .get("x-firecrab-controller-id")
                .and_then(|value| value.to_str().ok())
                == Some(id.as_str())
        });
    let mut response = if request.method() == axum::http::Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else if !id_matches {
        (StatusCode::CONFLICT, Json(serde_json::json!({"error": "resource controller identity changed; reload this host's dashboard"}))).into_response()
    } else {
        next.run(request).await
    };
    if let Some(origin) = origin {
        let headers = response.headers_mut();
        headers.insert("access-control-allow-origin", origin);
        headers.insert("vary", HeaderValue::from_static("Origin"));
        headers.insert(
            "access-control-allow-methods",
            HeaderValue::from_static("GET, PUT, POST, OPTIONS"),
        );
        headers.insert(
            "access-control-allow-headers",
            HeaderValue::from_static("Content-Type, X-Firecrab-Controller-Id"),
        );
    }
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

fn router(home: PathBuf) -> Router {
    router_for(home, resource::Platform::native())
}

fn router_for(home: PathBuf, platform: resource::Platform) -> Router {
    router_with_registry(home, platform, Registry::skills_sh())
}

fn router_with_registry(
    home: PathBuf,
    platform: resource::Platform,
    skill_registry: Registry,
) -> Router {
    let controller = Arc::new(Controller {
        home,
        platform,
        agents: Mutex::new(()),
        editors: Mutex::new(()),
        skills: Mutex::new(()),
        skill_registry,
    });
    Router::new()
        .route(
            "/api/service/resources",
            get(get_resources).put(save_resources),
        )
        .route("/api/service/resources/apply", post(apply_resources))
        .merge(
            Router::new()
                .route("/api/service/agents/skills/search", get(search_skills))
                .route(
                    "/api/service/agents/microvms/{vm}/skills",
                    get(vm_skills).post(change_vm_skills),
                )
                .route("/api/service/agents/microvms/{vm}/rules", get(vm_rules))
                .route(
                    "/api/service/agents/microvms/{vm}/rules/refine",
                    post(refine_vm_rule),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/rules/apply",
                    post(apply_vm_rule),
                )
                .route("/api/service/agents/microvms/{vm}", get(vm_agent_status))
                .route(
                    "/api/service/agents/microvms/{vm}/github",
                    post(connect_vm_github),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/github/repositories",
                    get(vm_github_repositories).post(create_vm_github_repository),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/codex/models",
                    get(vm_codex_models),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/project",
                    post(setup_vm_project),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/project/files",
                    get(vm_project_files),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/tasks",
                    post(start_vm_agent_task),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/prepare",
                    post(prepare_vm_agents),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/login",
                    post(login_vm_agent),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/{id}",
                    get(inspect_vm_agent),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/{id}/terminal",
                    get(vm_agent_terminal),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/{id}/input",
                    post(input_vm_agent),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/{id}/close",
                    post(close_vm_agent),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/{id}/name",
                    post(rename_vm_agent),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/{id}/resume",
                    post(resume_vm_agent_task),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/{id}/automation",
                    get(vm_automation_status).post(run_vm_automation),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/{id}/editor",
                    get(vm_editor_status).post(open_vm_editor),
                )
                .route(
                    "/api/service/agents/microvms/{vm}/{id}/editor/stop",
                    post(stop_vm_editor),
                )
                .route("/api/service/agents", get(agent_status))
                .route("/api/service/agents/tasks", post(start_agent_task))
                .route("/api/service/agents/{id}", get(inspect_agent))
                .route("/api/service/agents/{id}/input", post(input_agent))
                .route("/api/service/agents/{id}/close", post(close_agent))
                .layer(DefaultBodyLimit::max(32768)),
        )
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn_with_state(controller.clone(), guard))
        .with_state(controller)
}

/// Run blocking skill and rule work off the async runtime, mapping a join failure like other routes.
async fn blocking<F>(work: F) -> Result<Json<serde_json::Value>, Error>
where
    F: FnOnce() -> Result<serde_json::Value, Error> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Error::Invalid(e.to_string()))?
        .map(Json)
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillQuery {
    q: String,
}
async fn search_skills(
    State(controller): State<Arc<Controller>>,
    Query(query): Query<SkillQuery>,
) -> Result<Json<serde_json::Value>, Error> {
    blocking(move || controller.skill_registry.search(&query.q)).await
}
async fn vm_skills(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, Error> {
    blocking(move || {
        super::agents::Runtime::for_vm(&controller.home, vm, false)?.installed_skills()
    })
    .await
}
async fn change_vm_skills(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
    Json(change): Json<super::agents::skills::Change>,
) -> Result<Json<serde_json::Value>, Error> {
    blocking(move || {
        let _lock = controller.skills.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, false)?.change_skills(change)
    })
    .await
}
async fn vm_rules(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, Error> {
    blocking(move || super::agents::Runtime::for_vm(&controller.home, vm, false)?.rules()).await
}
async fn refine_vm_rule(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
    Json(input): Json<super::agents::rules::Refine>,
) -> Result<Json<serde_json::Value>, Error> {
    blocking(move || {
        super::agents::Runtime::for_vm(&controller.home, vm, true)?.refine_rules(input)
    })
    .await
}
async fn apply_vm_rule(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
    Json(input): Json<super::agents::rules::Apply>,
) -> Result<Json<serde_json::Value>, Error> {
    blocking(move || {
        let _lock = controller.skills.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, false)?.apply_rules(input)
    })
    .await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct VmCli {
    cli: String,
    #[serde(default)]
    id: Option<uuid::Uuid>,
}
async fn vm_agent_status(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let mut value = super::agents::Runtime::for_vm(&controller.home, vm, true)?.status()?;
        value["controllerId"] = serde_json::json!(resource::controller_id(&controller.home)?);
        Ok(Json(value))
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn vm_automation_status(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        super::agents::Runtime::for_vm(&controller.home, vm, false)?
            .automation_status(id)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn vm_editor_status(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        super::agents::Runtime::for_vm(&controller.home, vm, false)?
            .editor_status(id)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn open_vm_editor(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller
            .editors
            .lock()
            .map_err(|_| Error::Invalid("Editor connection lock is unavailable.".into()))?;
        super::agents::Runtime::for_vm(&controller.home, vm, false)?
            .open_editor(id)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn stop_vm_editor(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller
            .editors
            .lock()
            .map_err(|_| Error::Invalid("Editor connection lock is unavailable.".into()))?;
        super::agents::Runtime::for_vm(&controller.home, vm, false)?
            .stop_editor(id)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn run_vm_automation(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
    Json(input): Json<super::agents::automation::Request>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .automate(id, input)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn start_vm_agent_task(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
    Json(input): Json<super::agents::TaskStart>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .start_task(input)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn connect_vm_github(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .connect_github()
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn vm_github_repositories(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .github_repositories()
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn vm_codex_models(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .codex_models()
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn create_vm_github_repository(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
    Json(input): Json<super::agents::github::CreateRepository>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .create_github_repository(input)
            .map(Json)
    })
    .await
    .map_err(|error| Error::Invalid(error.to_string()))?
}

async fn setup_vm_project(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
    Json(input): Json<super::agents::project::Setup>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .setup_project(input)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesQuery {
    #[serde(default)]
    path: String,
}
async fn vm_project_files(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
    Query(query): Query<FilesQuery>,
) -> Result<Json<serde_json::Value>, Error> {
    blocking(move || {
        super::agents::Runtime::for_vm(&controller.home, vm, false)?.project_files(&query.path)
    })
    .await
}
async fn prepare_vm_agents(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
    Json(input): Json<VmCli>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, false)?
            .prepare(&input.cli)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn login_vm_agent(
    State(controller): State<Arc<Controller>>,
    RoutePath(vm): RoutePath<uuid::Uuid>,
    Json(input): Json<VmCli>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .login(input.id.unwrap_or_else(uuid::Uuid::new_v4), &input.cli)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn resume_vm_agent_task(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .deliver_task(id)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn inspect_vm_agent(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .inspect(id)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}

async fn vm_agent_terminal(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
    Query(query): Query<super::agents::terminal::Attach>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, Error> {
    // A WebSocket GET grants write access. It needs the same identity check as POST.
    if !headers.contains_key("origin") {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    if resource::controller_id(&controller.home)? != query.controller {
        return Ok((StatusCode::CONFLICT, Json(serde_json::json!({"error":"Resource controller identity changed; reload this host's dashboard."}))).into_response());
    }
    query.validate()?;
    let command = tokio::task::spawn_blocking(move || {
        super::agents::Runtime::for_vm(&controller.home, vm, true)?.terminal_command(id, &query)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))??;
    Ok(ws
        .max_message_size(400000)
        .max_frame_size(400000)
        .on_upgrade(move |socket| super::agents::terminal::bridge(socket, command))
        .into_response())
}
async fn input_vm_agent(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
    Json(input): Json<super::agents::Input>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .input(id, input)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionName {
    name: String,
}
async fn rename_vm_agent(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
    Json(input): Json<SessionName>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .rename(id, &input.name)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}
async fn close_vm_agent(
    State(controller): State<Arc<Controller>>,
    RoutePath((vm, id)): RoutePath<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::for_vm(&controller.home, vm, true)?
            .close(id)
            .map(Json)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
}

async fn agent_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let mut status = super::agents::Runtime::new(&controller.home).status()?;
        status["controllerId"] = serde_json::json!(resource::controller_id(&controller.home)?);
        Ok::<_, Error>(status)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
    .map(Json)
}
async fn inspect_agent(
    State(controller): State<Arc<Controller>>,
    RoutePath(id): RoutePath<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || super::agents::Runtime::new(&controller.home).inspect(id))
        .await
        .map_err(|e| Error::Invalid(e.to_string()))?
        .map(Json)
}
async fn start_agent_task(
    State(controller): State<Arc<Controller>>,
    Json(input): Json<super::agents::TaskStart>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::new(&controller.home).start_task(input)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
    .map(Json)
}
async fn input_agent(
    State(controller): State<Arc<Controller>>,
    RoutePath(id): RoutePath<uuid::Uuid>,
    Json(input): Json<super::agents::Input>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        super::agents::Runtime::new(&controller.home).input(id, input)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
    .map(Json)
}
async fn close_agent(
    State(controller): State<Arc<Controller>>,
    RoutePath(id): RoutePath<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let _lock = controller.agents.try_lock().map_err(|_| Error::Busy)?;
        super::agents::Runtime::new(&controller.home).close(id)
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
    .map(Json)
}

async fn get_resources(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<ManagerResourceStatus>, Error> {
    tokio::task::spawn_blocking(move || controller.platform.status(&controller.home))
        .await
        .map_err(|error| Error::Invalid(error.to_string()))?
        .map(Json)
}

async fn save_resources(
    State(controller): State<Arc<Controller>>,
    Json(settings): Json<ManagerResources>,
) -> Result<Json<ManagerResourceStatus>, Error> {
    tokio::task::spawn_blocking(move || {
        controller.platform.save(&controller.home, &settings)?;
        controller.platform.status(&controller.home)
    })
    .await
    .map_err(|error| Error::Invalid(error.to_string()))?
    .map(Json)
}

async fn apply_resources(
    State(controller): State<Arc<Controller>>,
    Json(body): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<ManagerResourceOperation>), Error> {
    if body != serde_json::json!({}) {
        return Err(Error::Invalid("apply expects an empty JSON object".into()));
    }
    tokio::task::spawn_blocking(move || spawn_apply(&controller.home, controller.platform))
        .await
        .map_err(|error| Error::Invalid(error.to_string()))?
        .map(|operation| (StatusCode::ACCEPTED, Json(operation)))
}

fn spawn_apply(
    home: &std::path::Path,
    platform: resource::Platform,
) -> Result<ManagerResourceOperation, Error> {
    spawn_worker(home, platform, Command::new(std::env::current_exe()?))
}

fn spawn_worker(
    home: &std::path::Path,
    platform: resource::Platform,
    mut command: Command,
) -> Result<ManagerResourceOperation, Error> {
    let _lock = resource::lock(home)?;
    let status = platform.status(home)?;
    if status
        .operation
        .is_some_and(|operation| operation.state == "applying")
    {
        return Err(Error::Busy);
    }
    let operation = ManagerResourceOperation {
        id: uuid::Uuid::new_v4().to_string(),
        state: "applying".into(),
        error: None,
    };
    resource::write(&home.join("runtime/resource-operation.json"), &operation)?;
    let log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(home.join("runtime/resource-apply.log"))?;
    command
        .args(["service", "resource-apply", &operation.id])
        .env("FIRECRAB_MICROMANAGER_HOME", home)
        .stdin(Stdio::null())
        .stderr(log.try_clone()?)
        .stdout(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Windows apply restarts WSL while leaving the native controller's
        // scheduled task alive. The worker does not inherit a console.
        command.creation_flags(0x00000200 | 0x00000008);
    }
    drop(_lock);
    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(operation)
        }
        Err(error) => {
            resource::write(
                &home.join("runtime/resource-operation.json"),
                &ManagerResourceOperation {
                    state: "failed".into(),
                    error: Some(error.to_string()),
                    ..operation
                },
            )?;
            Err(error.into())
        }
    }
}

pub fn run(home: Option<PathBuf>) -> Result<i32, Error> {
    let home = match home {
        Some(home) => home,
        None => resource::home()?,
    };
    if !home.is_absolute() || !home.is_dir() {
        return Err(Error::Invalid(
            "resource server requires an existing absolute managed home".into(),
        ));
    }
    let port = super::dev::instance::resource_port(&home)?;
    // Only one controller may own a managed home. The TCP bind also prevents
    // a second managed home from silently controlling the first one's port.
    fs::create_dir_all(home.join("runtime"))?;
    let server_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("runtime/resource-server.lock"))?;
    fs2::FileExt::try_lock_exclusive(&server_lock).map_err(|_| Error::Busy)?;
    #[cfg(target_os = "windows")]
    let holder = {
        match resource::host_capacity(&home)
            .and_then(|host| super::windows::resource::defaults(&home, &host))
        {
            Ok(current) => resource::write(&home.join("runtime/resources-active.json"), &current)?,
            Err(error) => eprintln!("[WARNING] could not record active WSL resources: {error}"),
        }
        start_windows_holder(home.clone())
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        axum::serve(listener, router(home)).await
    });
    #[cfg(target_os = "windows")]
    {
        holder.0.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = holder.1.join();
    }
    result?;
    Ok(0)
}

#[cfg(target_os = "windows")]
fn start_windows_holder(
    home: PathBuf,
) -> (
    Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    use std::os::windows::process::CommandExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    let handle = std::thread::spawn(move || {
        let mut child: Option<std::process::Child> = None;
        while !flag.load(Ordering::Relaxed) {
            if child
                .as_mut()
                .is_some_and(|child| child.try_wait().ok().flatten().is_some())
            {
                child = None;
            }
            if child.is_none() {
                // Apply holds this lock throughout resize and boot. Do not
                // reboot WSL in the middle of its offline disk operation.
                if let Ok(_lock) = resource::lock(&home) {
                    child = Command::new("wsl.exe")
                        .args([
                            "-d",
                            super::windows::DISTRO_NAME,
                            "-u",
                            "root",
                            "--exec",
                            "sleep",
                            "infinity",
                        ])
                        .creation_flags(0x08000000)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .ok();
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    });
    (stopped, handle)
}

#[cfg(test)]
mod tests;
