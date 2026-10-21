//! The browser's Host screen controls the outer VM through this native service.
//! Bind only loopback and reject foreign Host/Origin headers, including DNS rebinding.
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use firecrab_api_types::{ManagerResourceOperation, ManagerResourceStatus, ManagerResources};

use super::resource::{self, Error};

struct Controller {
    home: PathBuf,
    platform: resource::Platform,
}

const PORT: u16 = 5524;
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

fn allowed(headers: &HeaderMap) -> bool {
    let host = headers.get("host").and_then(|value| value.to_str().ok());
    let good_host = host.is_some_and(|host| host == "127.0.0.1:5524" || host == "localhost:5524");
    let good_origin = headers
        .get("origin")
        .is_none_or(|value| value.to_str().is_ok_and(|origin| ORIGINS.contains(&origin)));
    good_host && good_origin
}

async fn guard(
    State(controller): State<Arc<Controller>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if !allowed(request.headers()) {
        return StatusCode::FORBIDDEN.into_response();
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
    let controller = Arc::new(Controller { home, platform });
    Router::new()
        .route(
            "/api/service/resources",
            get(get_resources).put(save_resources),
        )
        .route("/api/service/resources/apply", post(apply_resources))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn_with_state(controller.clone(), guard))
        .with_state(controller)
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
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, PORT)).await?;
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
