use super::*;
use socket2::{Domain, Socket, Type};
use std::net::TcpListener;

const VITE: &str = r#"
import http from 'node:http';
import {readFileSync} from 'node:fs';
export async function createServer({root,server:options,plugins}) {
  const middleware=[];
  const server=http.createServer((request,response)=>{
    for(const [prefix,handler] of middleware) {
      if(request.url.startsWith(prefix)) {
        request.url=request.url.slice(prefix.length)||'/';
        handler(request,response); return;
      }
    }
    if(request.url==='/api/host') response.statusCode=Number(readFileSync(root+'/api-status','utf8'));
    response.end(root);
  });
  const api = {
    middlewares:{use:(prefix,handler)=>middleware.push([prefix,handler])},
    listen:()=>new Promise((resolve,reject)=>{server.once('error',reject);server.listen(options.port,options.host,resolve)}),
    close:()=>new Promise(resolve=>server.close(resolve)),
    printUrls:()=>{},
  };
  for (const plugin of plugins) plugin.configureServer(api);
  return api;
}
"#;

struct Fixture {
    source: tempfile::TempDir,
    home: tempfile::TempDir,
    port: u16,
}

impl Fixture {
    fn new() -> Self {
        let source = tempfile::Builder::new()
            .prefix("dashboard 소스 ")
            .tempdir()
            .unwrap();
        let frontend = source.path().join("firecrab-frontend");
        let vite = frontend.join("node_modules/vite");
        fs::create_dir_all(&vite).unwrap();
        fs::write(frontend.join("package.json"), "{}").unwrap();
        fs::write(frontend.join("package-lock.json"), "{}").unwrap();
        fs::write(frontend.join("api-status"), "200").unwrap();
        fs::write(
            vite.join("package.json"),
            r#"{"name":"vite","type":"module","exports":"./index.mjs"}"#,
        )
        .unwrap();
        fs::write(vite.join("index.mjs"), VITE).unwrap();
        fs::write(
            frontend.join("node_modules/.firecrab-dev-dependencies"),
            dependency_revision(&frontend).unwrap(),
        )
        .unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Self {
            source,
            home: tempfile::tempdir().unwrap(),
            port,
        }
    }

    fn source(&self) -> PathBuf {
        prepare(self.source.path()).unwrap()
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn start(&self) -> Result<(), Error> {
        start(&self.source(), self.home.path(), self.port)
    }
    fn start_private(&self, instance: &super::super::instance::Instance) -> Result<(), Error> {
        start(&self.source(), &instance.managed_home, self.port).inspect_err(|_| {
            if let Ok(log) =
                fs::read_to_string(instance.managed_home.join("runtime/dev-dashboard.log"))
            {
                eprintln!("private dashboard startup log:\n{log}");
            }
        })
    }
    fn state(&self) -> Session {
        read_session(self.home.path()).unwrap().unwrap()
    }
    fn private_instance(
        &self,
        id: &str,
        api_port: u16,
        resource_port: u16,
    ) -> super::super::instance::Instance {
        let mut instance =
            super::super::instance::test_instance(self.home.path(), id, api_port, resource_port);
        instance.source = self.source();
        fs::write(
            instance.managed_home.join("dev-instance.json"),
            serde_json::to_vec(&instance).unwrap(),
        )
        .unwrap();
        instance
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = stop(self.home.path());
    }
}

struct DashboardGuard(Session);

impl Drop for DashboardGuard {
    fn drop(&mut self) {
        if let Ok(client) = client() {
            let _ = stop_session(&client, &self.0, &self.0.url());
        }
    }
}

#[test]
fn starts_reuses_stops_and_resumes_the_selected_checkout() {
    let fixture = Fixture::new();
    fixture.start().unwrap();
    let original = fixture.state();
    fixture.start().unwrap();
    assert_eq!(fixture.state(), original);
    assert_eq!(
        client()
            .unwrap()
            .get(fixture.url())
            .send()
            .unwrap()
            .text()
            .unwrap(),
        fixture.source().join("firecrab-frontend").to_str().unwrap()
    );
    assert_eq!(source(fixture.home.path()).unwrap(), fixture.source());
    assert_eq!(port(fixture.home.path()).unwrap(), fixture.port);
    stop(fixture.home.path()).unwrap();
    stop(fixture.home.path()).unwrap();
    assert!(!running(&client().unwrap(), &original, &fixture.url()));
    start(
        &source(fixture.home.path()).unwrap(),
        fixture.home.path(),
        port(fixture.home.path()).unwrap(),
    )
    .unwrap();
    assert_ne!(fixture.state().session, original.session);
    assert_eq!(fixture.state().port, fixture.port);
}

#[test]
fn different_managed_homes_keep_their_dashboards_and_stop_independently() {
    let first = Fixture::new();
    let second = Fixture::new();
    first.start().unwrap();
    second.start().unwrap();
    let selected = second.state();
    assert_ne!(first.state().session, selected.session);
    stop(first.home.path()).unwrap();
    assert!(!listening(&first.url()));
    assert!(running(&client().unwrap(), &selected, &second.url()));
    clean(first.home.path()).unwrap();
    assert_eq!(second.state(), selected);
    assert!(running(&client().unwrap(), &selected, &second.url()));
}

#[test]
fn a_private_instance_passes_its_backend_ports_to_the_owned_server() {
    let fixture = Fixture::new();
    let instance = fixture.private_instance("0123456789abcdef", 45121, 45122);
    // Canonical Windows homes use the verbatim prefix that Node must not receive
    // as its ESM entry argument. This fixture must retain that production shape.
    #[cfg(windows)]
    assert!(matches!(
        instance.managed_home.components().next(),
        Some(std::path::Component::Prefix(prefix)) if prefix.kind().is_verbatim()
    ));
    fixture.start_private(&instance).unwrap();
    let selected = read_session(&instance.managed_home).unwrap().unwrap();
    assert_eq!(selected.api_port, 45121);
    assert_eq!(selected.resource_port, 45122);
    assert!(selected.isolated);
    assert!(running(&client().unwrap(), &selected, &fixture.url()));
    stop(&instance.managed_home).unwrap();
}

#[test]
fn copied_sessions_cannot_control_primary_or_other_private_dashboards() {
    let primary = Fixture::new();
    let first = Fixture::new();
    let second = Fixture::new();
    let first_instance = first.private_instance("0123456789abcdef", 45121, 45122);
    let second_instance = second.private_instance("fedcba9876543210", 45123, 45124);
    primary.start().unwrap();
    let primary_server = DashboardGuard(primary.state());
    first.start_private(&first_instance).unwrap();
    let first_server = DashboardGuard(read_session(&first_instance.managed_home).unwrap().unwrap());
    second.start_private(&second_instance).unwrap();
    let second_server = DashboardGuard(
        read_session(&second_instance.managed_home)
            .unwrap()
            .unwrap(),
    );
    let primary_bytes = serde_json::to_vec(&primary_server.0).unwrap();
    let first_bytes = serde_json::to_vec(&first_server.0).unwrap();
    let second_bytes = serde_json::to_vec(&second_server.0).unwrap();
    let primary_home = primary.home.path().to_owned();
    let cases = [
        (
            &first_instance.managed_home,
            first.source(),
            first.port,
            &first_bytes,
            &primary_bytes,
        ),
        (
            &second_instance.managed_home,
            second.source(),
            second.port,
            &second_bytes,
            &first_bytes,
        ),
        (
            &primary_home,
            primary.source(),
            primary.port,
            &primary_bytes,
            &first_bytes,
        ),
    ];
    let client = client().unwrap();
    for (home, source, selected_port, original, foreign) in cases {
        let record = home.join("runtime/dev-dashboard.json");
        assert!(check_port(home, selected_port).is_ok());
        fs::write(&record, foreign).unwrap();
        assert!(stop(home).is_err());
        assert!(clean(home).is_err());
        assert!(start(&source, home, selected_port).is_err());
        assert!(check_port(home, selected_port).is_err());
        assert!(port(home).is_err());
        assert_eq!(fs::read(&record).unwrap(), *foreign);
        for server in [&primary_server, &first_server, &second_server] {
            assert!(running(&client, &server.0, &server.0.url()));
        }
        fs::write(record, original).unwrap();
    }
}

#[test]
fn a_private_dashboard_rejects_a_different_source_before_stopping_its_server() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    let instance = fixture.private_instance("0123456789abcdef", 45121, 45122);
    fixture.start_private(&instance).unwrap();
    let server = DashboardGuard(read_session(&instance.managed_home).unwrap().unwrap());
    let record = instance.managed_home.join("runtime/dev-dashboard.json");
    let before = fs::read(&record).unwrap();
    assert!(start(&other.source(), &instance.managed_home, fixture.port).is_err());
    assert_eq!(fs::read(&record).unwrap(), before);
    assert!(running(&client().unwrap(), &server.0, &server.0.url()));
    let mut foreign: Session = serde_json::from_slice(&before).unwrap();
    foreign.source = other.source();
    fs::write(&record, serde_json::to_vec(&foreign).unwrap()).unwrap();
    assert!(stop(&instance.managed_home).is_err());
    assert!(clean(&instance.managed_home).is_err());
    assert!(check_port(&instance.managed_home, fixture.port).is_err());
    assert!(start(&fixture.source(), &instance.managed_home, fixture.port).is_err());
    assert!(running(&client().unwrap(), &server.0, &server.0.url()));
    fs::write(record, before).unwrap();
}

#[test]
fn check_port_never_creates_state_and_preserves_foreign_listeners() {
    let fixture = Fixture::new();
    assert!(check_port(fixture.home.path(), fixture.port).is_ok());
    assert!(!fixture.home.path().join("runtime").exists());
    let listener = TcpListener::bind(("127.0.0.1", fixture.port)).unwrap();
    assert!(check_port(fixture.home.path(), fixture.port).is_err());
    assert!(!fixture.home.path().join("runtime").exists());
    assert!(listener.local_addr().is_ok());
}

#[test]
fn restarts_the_dashboard_after_dependencies_are_reinstalled() {
    let fixture = Fixture::new();
    fixture.start().unwrap();
    let original = fixture.state();
    let frontend = fixture.source().join("firecrab-frontend");
    fs::write(
        frontend.join("node_modules/.firecrab-dev-dependencies"),
        format!(
            "{}\n{}",
            dependency_revision(&frontend).unwrap(),
            Uuid::new_v4()
        ),
    )
    .unwrap();
    fixture.start().unwrap();
    let restarted = fixture.state();
    assert_eq!(restarted.source, original.source);
    assert_ne!(restarted.session, original.session);
    assert_ne!(restarted.dependencies, original.dependencies);
    assert!(!running(&client().unwrap(), &original, &fixture.url()));
}

#[test]
fn clean_stops_the_dashboard_and_preserves_its_source_and_port() {
    let fixture = Fixture::new();
    fixture.start().unwrap();
    let state = fixture.state();
    let runtime = fixture.home.path().join("runtime");
    fs::write(runtime.join("other-state.json"), "preserved").unwrap();
    for _ in 0..2 {
        clean(fixture.home.path()).unwrap();
        assert!(!running(&client().unwrap(), &state, &fixture.url()));
        assert_eq!(fixture.state(), state);
        assert_eq!(source(fixture.home.path()).unwrap(), fixture.source());
        assert_eq!(port(fixture.home.path()).unwrap(), fixture.port);
        for name in ["dev-dashboard.mjs", "dev-dashboard.log"] {
            assert!(!runtime.join(name).exists());
        }
        assert!(runtime.join("dev-dashboard.lock").is_file());
        assert_eq!(
            fs::read_to_string(runtime.join("other-state.json")).unwrap(),
            "preserved"
        );
        assert!(
            fixture
                .source()
                .join("firecrab-frontend/node_modules/vite/index.mjs")
                .is_file()
        );
    }
}

#[test]
fn a_cleaned_private_dashboard_resumes_its_saved_source_and_port() {
    let fixture = Fixture::new();
    let instance = fixture.private_instance("0123456789abcdef", 45121, 45122);
    fixture.start_private(&instance).unwrap();
    let original = read_session(&instance.managed_home).unwrap().unwrap();
    clean(&instance.managed_home).unwrap();
    assert!(!listening(&fixture.url()));
    assert!(has_session(&instance.managed_home).unwrap());
    // The process cwd is outside this fixture; resume must use the canonical
    // saved checkout rather than treating cwd as the dashboard source.
    let selected = source(&instance.managed_home).unwrap();
    let selected_port = port(&instance.managed_home).unwrap();
    assert_eq!(selected, instance.source);
    assert_eq!(selected_port, fixture.port);
    start(&selected, &instance.managed_home, selected_port).unwrap();
    let resumed = DashboardGuard(read_session(&instance.managed_home).unwrap().unwrap());
    assert_ne!(resumed.0.session, original.session);
    assert_eq!(resumed.0.source, original.source);
    assert_eq!(resumed.0.port, original.port);
    assert!(running(&client().unwrap(), &resumed.0, &fixture.url()));
    assert!(!instance.primary_home.exists());
}

#[test]
fn a_private_instance_without_dashboard_settings_cannot_resume_the_current_directory() {
    let fixture = Fixture::new();
    let instance = fixture.private_instance("0123456789abcdef", 45121, 45122);
    assert!(!has_session(&instance.managed_home).unwrap());
    assert!(
        source(&instance.managed_home)
            .unwrap_err()
            .to_string()
            .contains("run service dev --port PORT first")
    );
    assert!(
        !instance
            .managed_home
            .join("runtime/dev-dashboard.json")
            .exists()
    );
    assert!(!instance.primary_home.exists());
}

#[test]
fn clean_preserves_dashboard_files_when_ownership_state_is_invalid_or_locked() {
    let home = tempfile::tempdir().unwrap();
    let runtime = home.path().join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    fs::write(runtime.join("dev-dashboard.json"), "invalid session").unwrap();
    fs::write(runtime.join("dev-dashboard.log"), "preserved").unwrap();
    assert!(matches!(clean(home.path()), Err(Error::State(_))));
    assert_eq!(
        fs::read_to_string(runtime.join("dev-dashboard.log")).unwrap(),
        "preserved"
    );
    let _lock = lock(home.path()).unwrap();
    assert!(
        clean(home.path())
            .unwrap_err()
            .to_string()
            .contains("another development dashboard")
    );
    assert!(runtime.join("dev-dashboard.json").exists());
}

#[test]
fn refuses_unauthorized_shutdown_and_stale_session_ownership() {
    let fixture = Fixture::new();
    fixture.start().unwrap();
    let client = client().unwrap();
    assert_eq!(
        client
            .post(format!("{}{CONTROL}stop", fixture.url()))
            .send()
            .unwrap()
            .status(),
        403
    );
    let stale = Session {
        source: fixture.source(),
        session: Uuid::new_v4(),
        dependencies: fixture.state().dependencies,
        port: fixture.port,
        ..fixture.state()
    };
    stop_session(&client, &stale, &fixture.url()).unwrap();
    assert!(running(&client, &fixture.state(), &fixture.url()));
}

#[test]
fn switches_checkouts_by_stopping_only_the_previous_owned_server() {
    let first = Fixture::new();
    let second = Fixture::new();
    first.start().unwrap();
    let original = first.state();
    start(&second.source(), first.home.path(), first.port).unwrap();
    assert!(!running(&client().unwrap(), &original, &first.url()));
    assert_eq!(first.state().source, second.source());
}

#[test]
fn changing_ports_stops_the_previous_dashboard_and_remembers_the_new_port() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    fixture.start().unwrap();
    let original = fixture.state();
    start(&fixture.source(), fixture.home.path(), other.port).unwrap();
    let selected = fixture.state();
    assert_eq!(selected.port, other.port);
    assert!(!listening(&original.url()));
    assert!(running(&client().unwrap(), &selected, &selected.url()));
    stop(fixture.home.path()).unwrap();
    assert!(!listening(&selected.url()));
    assert_eq!(port(fixture.home.path()).unwrap(), other.port);
}

#[test]
fn old_sessions_and_missing_state_use_the_default_port() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(port(home.path()).unwrap(), 8080);
    let runtime = home.path().join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    fs::write(
        runtime.join("dev-dashboard.json"),
        serde_json::json!({"source": home.path(), "session": Uuid::new_v4()}).to_string(),
    )
    .unwrap();
    let state = read_session(home.path()).unwrap().unwrap();
    assert_eq!(state.port, 8080);
    assert_eq!(state.api_port, 5523);
    assert_eq!(state.resource_port, 5524);
    assert!(!state.isolated);
    assert_eq!(state.url(), "http://127.0.0.1:8080");
    assert_eq!(port(home.path()).unwrap(), 8080);
}

#[test]
fn port_probe_distinguishes_listening_and_closed_ports() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    assert!(listening(&format!(
        "http://{}",
        listener.local_addr().unwrap()
    )));
    // A bound socket that never listens refuses connections and keeps its port
    // from parallel tests; releasing and rebinding a port races with them.
    let closed = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
    closed
        .bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())
        .unwrap();
    let address = closed.local_addr().unwrap().as_socket().unwrap();
    assert!(!listening(&format!("http://{address}")));
}

#[test]
fn occupied_ports_are_errors_and_the_other_listener_remains_open() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind(("127.0.0.1", fixture.port)).unwrap();
    assert!(
        fixture
            .start()
            .unwrap_err()
            .to_string()
            .contains("already in use")
    );
    assert!(read_session(fixture.home.path()).unwrap().is_none());
    assert!(listener.local_addr().is_ok());
}

#[test]
fn an_occupied_new_port_preserves_the_running_dashboard_and_selected_port() {
    let fixture = Fixture::new();
    fixture.start().unwrap();
    let original = fixture.state();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let occupied = listener.local_addr().unwrap().port();
    assert!(
        start(&fixture.source(), fixture.home.path(), occupied)
            .unwrap_err()
            .to_string()
            .contains("already in use")
    );
    assert_eq!(fixture.state(), original);
    assert!(running(&client().unwrap(), &original, &original.url()));
    assert!(listener.local_addr().is_ok());
}

#[test]
fn dependency_or_proxy_failure_never_reports_a_ready_dashboard() {
    let fixture = Fixture::new();
    fs::write(fixture.source().join("firecrab-frontend/api-status"), "503").unwrap();
    assert!(fixture.start().is_err());
    stop(fixture.home.path()).unwrap();
    fs::write(
        fixture
            .source()
            .join("firecrab-frontend/node_modules/vite/index.mjs"),
        "throw new Error('fixture startup failure')",
    )
    .unwrap();
    assert!(
        fixture
            .start()
            .unwrap_err()
            .to_string()
            .contains("failed to start")
    );
    assert!(
        fs::read_to_string(fixture.home.path().join("runtime/dev-dashboard.log"))
            .unwrap()
            .contains("fixture startup failure")
    );
}

#[test]
fn invalid_checkouts_and_concurrent_operations_are_rejected() {
    let fixture = Fixture::new();
    let _lock = lock(fixture.home.path()).unwrap();
    assert!(
        fixture
            .start()
            .unwrap_err()
            .to_string()
            .contains("another development dashboard")
    );
    let empty = tempfile::tempdir().unwrap();
    assert!(
        prepare(empty.path())
            .unwrap_err()
            .to_string()
            .contains("source is missing")
    );
    stop(empty.path()).unwrap();
}

#[test]
fn installs_missing_locked_dependencies_and_reports_invalid_locks() {
    let fixture = Fixture::new();
    let frontend = fixture.source.path().join("firecrab-frontend");
    fs::rename(
        frontend.join("node_modules/vite"),
        frontend.join("fixture-vite"),
    )
    .unwrap();
    fs::write(frontend.join("package.json"), r#"{"name":"dashboard-fixture","version":"1.0.0","dependencies":{"vite":"file:./fixture-vite"}}"#).unwrap();
    fs::write(frontend.join("package-lock.json"), r#"{"name":"dashboard-fixture","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"dashboard-fixture","version":"1.0.0","dependencies":{"vite":"file:./fixture-vite"}},"node_modules/vite":{"resolved":"fixture-vite","link":true},"fixture-vite":{"name":"vite","version":"1.0.0"}}}"#).unwrap();
    prepare(fixture.source.path()).unwrap();
    assert!(frontend.join("node_modules/vite/package.json").is_file());
    let sentinel = frontend.join("node_modules/keep-installed-dependencies");
    fs::write(&sentinel, "preserved").unwrap();
    prepare(fixture.source.path()).unwrap();
    assert!(sentinel.is_file());

    fs::remove_file(frontend.join("node_modules/.firecrab-dev-dependencies")).unwrap();
    prepare(fixture.source.path()).unwrap();
    assert!(!sentinel.exists());
    fs::write(&sentinel, "stale").unwrap();

    // Reproduce a branch update that adds Ghostty while Vite is already installed.
    let ghostty = frontend.join("fixture-ghostty");
    fs::create_dir(&ghostty).unwrap();
    fs::write(
        ghostty.join("package.json"),
        r#"{"name":"ghostty-web","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(ghostty.join("ghostty-vt.wasm"), "fixture wasm").unwrap();
    fs::write(frontend.join("package.json"), r#"{"name":"dashboard-fixture","version":"1.0.0","dependencies":{"vite":"file:./fixture-vite","ghostty-web":"file:./fixture-ghostty"}}"#).unwrap();
    fs::write(frontend.join("package-lock.json"), r#"{"name":"dashboard-fixture","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"dashboard-fixture","version":"1.0.0","dependencies":{"vite":"file:./fixture-vite","ghostty-web":"file:./fixture-ghostty"}},"node_modules/vite":{"resolved":"fixture-vite","link":true},"fixture-vite":{"name":"vite","version":"1.0.0"},"node_modules/ghostty-web":{"resolved":"fixture-ghostty","link":true},"fixture-ghostty":{"name":"ghostty-web","version":"1.0.0"}}}"#).unwrap();
    prepare(fixture.source.path()).unwrap();
    assert!(frontend.join("node_modules/vite/package.json").is_file());
    assert!(
        frontend
            .join("node_modules/ghostty-web/ghostty-vt.wasm")
            .is_file()
    );
    assert!(!sentinel.exists());

    // Manifest-only edits also invalidate the installation.
    fs::write(&sentinel, "stale").unwrap();
    let manifest = fs::read_to_string(frontend.join("package.json")).unwrap();
    fs::write(frontend.join("package.json"), format!("{manifest}\n")).unwrap();
    prepare(fixture.source.path()).unwrap();
    assert!(!sentinel.exists());

    fs::write(frontend.join("package-lock.json"), "{}").unwrap();
    assert!(
        prepare(fixture.source.path())
            .unwrap_err()
            .to_string()
            .contains("dependency installation failed")
    );
}

#[test]
fn wasm_build_failure_is_reported_before_starting_the_dashboard() {
    let fixture = Fixture::new();
    let frontend = fixture.source.path().join("firecrab-frontend");
    fs::write(frontend.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::create_dir(frontend.join("scripts")).unwrap();
    fs::write(frontend.join("scripts/wasm.mjs"), "process.exit(42)").unwrap();
    assert!(
        prepare(fixture.source.path())
            .unwrap_err()
            .to_string()
            .contains("dashboard WASM build failed")
    );
    assert!(
        !fixture
            .home
            .path()
            .join("runtime/dev-dashboard.json")
            .exists()
    );
}
