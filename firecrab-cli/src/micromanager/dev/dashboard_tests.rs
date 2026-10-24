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
        start_at(&self.source(), self.home.path(), self.port)
    }
    fn state(&self) -> Session {
        read_session(self.home.path()).unwrap().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = stop_at(self.home.path(), &self.url());
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
    stop_at(fixture.home.path(), &fixture.url()).unwrap();
    stop_at(fixture.home.path(), &fixture.url()).unwrap();
    assert!(!running(&client().unwrap(), &original, &fixture.url()));
    fixture.start().unwrap();
    assert_ne!(fixture.state().session, original.session);
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
fn clean_stops_the_dashboard_and_forgets_only_its_runtime_files() {
    let fixture = Fixture::new();
    fixture.start().unwrap();
    let state = fixture.state();
    let runtime = fixture.home.path().join("runtime");
    fs::write(runtime.join("other-state.json"), "preserved").unwrap();
    for _ in 0..2 {
        clean_at(fixture.home.path(), &fixture.url()).unwrap();
        assert!(!running(&client().unwrap(), &state, &fixture.url()));
        assert!(read_session(fixture.home.path()).unwrap().is_none());
        for name in [
            "dev-dashboard.json",
            "dev-dashboard.mjs",
            "dev-dashboard.log",
        ] {
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
    start_at(&second.source(), first.home.path(), first.port).unwrap();
    assert!(!running(&client().unwrap(), &original, &first.url()));
    assert_eq!(first.state().source, second.source());
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
fn dependency_or_proxy_failure_never_reports_a_ready_dashboard() {
    let fixture = Fixture::new();
    fs::write(fixture.source().join("firecrab-frontend/api-status"), "503").unwrap();
    assert!(fixture.start().is_err());
    stop_at(fixture.home.path(), &fixture.url()).unwrap();
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
