use super::*;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Default)]
struct Fake {
    wakes: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
}
impl Backend for Fake {
    fn wake(&self, _: &Settings) -> Result<(), String> {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(30));
        if self.fail.load(Ordering::SeqCst) {
            Err("boot failed".into())
        } else {
            Ok(())
        }
    }
    fn stop(&self) -> Result<(), String> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn home() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("runtime")).unwrap();
    std::fs::write(root.path().join("runtime/sleeping"), b"idle").unwrap();
    let store = Store::new(root.path());
    let settings = Settings {
        autostart: false,
        ..Settings::default()
    };
    store.save(&store.load().unwrap(), settings).unwrap();
    root
}
async fn guest(
    busy: Arc<AtomicBool>,
    activity_valid: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (busy, valid, calls) = (
                Arc::clone(&busy),
                Arc::clone(&activity_valid),
                Arc::clone(&calls),
            );
            tokio::spawn(async move {
                let request = read_header(&mut stream).await.unwrap();
                if request.path == "/api/micromanager/activity" {
                    if valid.load(Ordering::SeqCst) {
                        json_reply(
                            &mut stream,
                            200,
                            &serde_json::json!({"busy":busy.load(Ordering::SeqCst)}).to_string(),
                        )
                        .await
                        .unwrap();
                    } else {
                        json_reply(&mut stream, 200, "{}").await.unwrap();
                    }
                } else {
                    calls.fetch_add(1, Ordering::SeqCst);
                    // Simulate a lost response after executing a mutation.
                    if request.path != "/drop-response" {
                        json_reply(&mut stream, 200, "{\"ok\":true}").await.unwrap();
                    }
                }
            });
        }
    });
    (addr, task)
}
async fn http(addr: std::net::SocketAddr, method: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(bytes).unwrap()
}
#[tokio::test]
async fn concurrent_work_wakes_once_and_manual_stop_blocks_requests() {
    let root = home();
    let fake = Fake::default();
    let controller = Controller::new(
        fake.clone(),
        root.path().into(),
        "127.0.0.1:1".parse().unwrap(),
    )
    .unwrap();
    assert!(controller.lease(false).await.is_err());
    assert_eq!(fake.wakes.load(Ordering::SeqCst), 0);
    let leases = futures_util::future::join_all((0..20).map(|_| controller.lease(true))).await;
    assert!(leases.iter().all(Result::is_ok));
    assert_eq!(fake.wakes.load(Ordering::SeqCst), 1);
    assert_eq!(controller.active.load(Ordering::SeqCst), 20);
    drop(leases);
    std::fs::write(root.path().join("runtime/manual-stop"), b"manual").unwrap();
    assert!(controller.lease(true).await.is_err());
    assert_eq!(fake.wakes.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn idle_requires_no_jobs_leases_or_host_work_and_valid_guest_report() {
    let root = home();
    let fake = Fake::default();
    let busy = Arc::new(AtomicBool::new(true));
    let valid = Arc::new(AtomicBool::new(true));
    let (addr, guest_task) = guest(
        Arc::clone(&busy),
        Arc::clone(&valid),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let controller = Controller::new(fake.clone(), root.path().into(), addr).unwrap();
    let lease = controller.lease(true).await.unwrap();
    controller.idle(Duration::ZERO).await;
    assert_eq!(fake.stops.load(Ordering::SeqCst), 0);
    drop(lease);
    controller.idle(Duration::ZERO).await;
    assert_eq!(fake.stops.load(Ordering::SeqCst), 0);
    busy.store(false, Ordering::SeqCst);
    valid.store(false, Ordering::SeqCst);
    controller.idle(Duration::ZERO).await;
    assert!(controller.status().detail.unwrap().contains("invalid"));
    valid.store(true, Ordering::SeqCst);
    let file = std::fs::File::create(root.path().join("runtime/host-work.lock")).unwrap();
    fs2::FileExt::lock_shared(&file).unwrap();
    controller.idle(Duration::ZERO).await;
    assert_eq!(fake.stops.load(Ordering::SeqCst), 0);
    drop(file);
    controller.idle(Duration::ZERO).await;
    assert_eq!(fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(controller.status().state, "sleeping");
    assert!(root.path().join("runtime/sleeping").exists());
    let lease = controller.lease(true).await.unwrap();
    assert_eq!(fake.wakes.load(Ordering::SeqCst), 2);
    drop(lease);
    guest_task.abort();
    controller.idle(Duration::ZERO).await;
    assert_eq!(fake.stops.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn real_proxy_does_not_wake_for_monitoring_or_retry_lost_mutations() {
    let root = home();
    let fake = Fake::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let (guest_addr, guest_task) = guest(
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(true)),
        Arc::clone(&calls),
    )
    .await;
    let controller = Controller::new(fake.clone(), root.path().into(), guest_addr).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let service = tokio::spawn(Arc::clone(&controller).serve(listener));
    assert!(http(addr, "GET", STATUS_PATH).await.contains("sleeping"));
    assert!(
        http(addr, "GET", "/api/vms")
            .await
            .starts_with("HTTP/1.1 503")
    );
    assert_eq!(fake.wakes.load(Ordering::SeqCst), 0);
    assert!(
        http(addr, "POST", "/api/vms/example/start")
            .await
            .starts_with("HTTP/1.1 200")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(http(addr, "POST", "/drop-response").await.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    tokio::time::timeout(Duration::from_secs(1), async {
        while controller.active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    controller.idle(Duration::ZERO).await;
    assert_eq!(controller.status().state, "sleeping");
    assert!(
        http(addr, "POST", "/api/vms/example/start")
            .await
            .starts_with("HTTP/1.1 200")
    );
    assert_eq!(fake.wakes.load(Ordering::SeqCst), 2);
    assert!(
        http(addr, "POST", "/api/micromanager/stop")
            .await
            .starts_with("HTTP/1.1 200")
    );
    assert!(
        http(addr, "POST", "/api/vms/example/start")
            .await
            .starts_with("HTTP/1.1 503")
    );
    assert_eq!(fake.wakes.load(Ordering::SeqCst), 2);
    assert!(
        http(addr, "POST", "/api/micromanager/start")
            .await
            .starts_with("HTTP/1.1 200")
    );
    assert_eq!(fake.wakes.load(Ordering::SeqCst), 3);
    service.abort();
    guest_task.abort();
}
#[tokio::test]
async fn failed_boot_returns_no_lease_and_sends_no_work() {
    let root = home();
    let fake = Fake::default();
    fake.fail.store(true, Ordering::SeqCst);
    let controller = Controller::new(
        fake.clone(),
        root.path().into(),
        "127.0.0.1:1".parse().unwrap(),
    )
    .unwrap();
    let failures = futures_util::future::join_all((0..20).map(|_| controller.lease(true))).await;
    assert!(failures.iter().all(Result::is_err));
    assert_eq!(fake.wakes.load(Ordering::SeqCst), 1);
    assert_eq!(controller.status().state, "failed");
    assert_eq!(controller.active.load(Ordering::SeqCst), 0);
    fake.fail.store(false, Ordering::SeqCst);
    assert!(controller.lease(true).await.is_ok());
}
#[tokio::test]
async fn host_shell_lease_and_websocket_tunnel_inhibit_sleep_until_disconnect() {
    let root = home();
    let fake = Fake::default();
    let (guest_addr, guest_task) = guest(
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let controller = Controller::new(fake.clone(), root.path().into(), guest_addr).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(Arc::clone(&controller).serve(listener));
    let mut shell = TcpStream::connect(addr).await.unwrap();
    shell
        .write_all(
            b"POST /api/micromanager/hold HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await
        .unwrap();
    let mut reply = [0; 512];
    let n = shell.read(&mut reply).await.unwrap();
    assert!(reply[..n].starts_with(b"HTTP/1.1 200"));
    controller.idle(Duration::ZERO).await;
    assert_eq!(fake.stops.load(Ordering::SeqCst), 0);
    drop(shell);
    tokio::time::sleep(Duration::from_millis(20)).await;
    controller.idle(Duration::ZERO).await;
    assert_eq!(fake.stops.load(Ordering::SeqCst), 1);
    // Console GETs are work requests, unlike ordinary polling GETs.
    assert!(
        http(addr, "GET", "/ws/vms/example/console")
            .await
            .starts_with("HTTP/1.1 200")
    );
    assert_eq!(fake.wakes.load(Ordering::SeqCst), 2);
    server.abort();
    guest_task.abort();
}
