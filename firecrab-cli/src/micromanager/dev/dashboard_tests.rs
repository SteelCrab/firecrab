use super::*;
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
fn port_probe_tracks_listener_lifecycle() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    assert!(listening(&url));
    drop(listener.accept().unwrap());
    drop(listener);
    assert!(!listening(&url));
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
    fs::remove_dir_all(frontend.join("node_modules")).unwrap();
    fs::write(frontend.join("package-lock.json"), "{}").unwrap();
    assert!(
        prepare(fixture.source.path())
            .unwrap_err()
            .to_string()
            .contains("dependency installation failed")
    );
}
