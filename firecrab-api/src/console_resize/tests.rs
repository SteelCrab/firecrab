use super::*;

fn size(rows: u16, cols: u16) -> Dimensions {
    Dimensions { rows, cols }
}

#[test]
fn smallest_grid_wins_and_disconnect_restores_the_survivor() {
    let hub = Arc::new(ResizeHub::new());
    let a = hub.register();
    let b = hub.register();
    let updates = hub.subscribe();
    a.report(size(50, 120));
    b.report(size(30, 140));
    assert_eq!(updates.borrow().target, Some(size(30, 120)));
    assert_eq!(updates.borrow().viewers, 2);
    let revision = updates.borrow().revision;
    hub.publish(revision, size(30, 120), true);
    // Drop the small viewer; its measurements must never constrain another session.
    drop(b);
    assert_eq!(updates.borrow().target, Some(size(50, 120)));
    assert_eq!(updates.borrow().status, ResizeStatus::Pending);
    drop(a);
    assert_eq!(updates.borrow().target, None);
    assert_eq!(updates.borrow().applied, None);
}

#[test]
fn invalid_and_duplicate_reports_do_not_change_the_target() {
    let hub = Arc::new(ResizeHub::new());
    let viewer = hub.register();
    viewer.report(size(24, 80));
    let updates = hub.subscribe();
    let initial = *updates.borrow();
    viewer.report(size(24, 80));
    viewer.report(size(0, 80));
    viewer.report(size(24, 1001));
    assert_eq!(*updates.borrow(), initial);
}

#[test]
fn stale_acknowledgement_cannot_claim_a_new_target_was_applied() {
    let hub = Arc::new(ResizeHub::new());
    let a = hub.register();
    a.report(size(24, 80));
    let updates = hub.subscribe();
    let old = *updates.borrow();
    a.report(size(40, 120));
    hub.publish(old.revision, size(24, 80), true);
    assert_eq!(updates.borrow().applied, None);
    assert_eq!(updates.borrow().status, ResizeStatus::Pending);
}

#[tokio::test]
async fn private_channel_checks_the_handshake_and_acknowledgement() {
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    let path = directory.path().join("v.sock");
    let server = tokio::net::UnixListener::bind(&path).unwrap();
    let task = tokio::spawn(async move {
        let (mut connection, _) = server.accept().await.unwrap();
        let mut connect = [0; 14];
        connection.read_exact(&mut connect).await.unwrap();
        assert_eq!(&connect, b"CONNECT 47100\n");
        connection.write_all(b"OK 1073741824\n").await.unwrap();
        let mut frame = [0; 8];
        connection.read_exact(&mut frame).await.unwrap();
        assert_eq!(&frame, b"FCR1\x00\x28\x00\x78");
        connection.write_all(b"FCA1\x00\x28\x00\x78").await.unwrap();
    });
    apply_size(path, size(40, 120)).await.unwrap();
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn missing_guest_support_is_reported_without_blocking_output_or_leaking_workers() {
    let hub = Arc::new(ResizeHub::new());
    let viewer = hub.register();
    let (exit, exited) = watch::channel(false);
    viewer.report(size(24, 80));
    let mut updates = hub.subscribe();
    let task = tokio::spawn(Arc::clone(&hub).synchronize(viewer.id, exited));
    updates
        .wait_for(|s| s.status == ResizeStatus::Unavailable)
        .await
        .unwrap();
    assert_eq!(updates.borrow().applied, None);
    exit.send(true).unwrap();
    task.await.unwrap();
    drop(viewer);
    assert_eq!(updates.borrow().viewers, 0);
}

#[tokio::test]
async fn resize_bursts_are_coalesced_and_a_disconnected_small_viewer_releases_its_size() {
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    let path = directory.path().join("v.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let (sent, mut received) = tokio::sync::mpsc::channel(8);
    let server = tokio::spawn(async move {
        loop {
            let (mut connection, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 14];
            connection.read_exact(&mut greeting).await.unwrap();
            connection.write_all(b"OK 1073741824\n").await.unwrap();
            let mut request = [0; 8];
            connection.read_exact(&mut request).await.unwrap();
            request[2] = b'A';
            connection.write_all(&request).await.unwrap();
            sent.send(Dimensions {
                rows: u16::from_be_bytes([request[4], request[5]]),
                cols: u16::from_be_bytes([request[6], request[7]]),
            })
            .await
            .unwrap();
        }
    });
    let hub = Arc::new(ResizeHub::new());
    hub.set_socket(path);
    let a = hub.register();
    let b = hub.register();
    let (exit, exited) = watch::channel(false);
    let worker = tokio::spawn(Arc::clone(&hub).synchronize(a.id, exited.clone()));
    a.report(size(50, 120));
    for cols in 80..100 {
        b.report(size(30, cols));
    }
    let mut changes = hub.subscribe();
    tokio::time::timeout(
        Duration::from_secs(3),
        changes.wait_for(|s| s.status == ResizeStatus::Applied),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(received.recv().await, Some(size(30, 99)));
    assert!(received.try_recv().is_err());
    drop(b);
    tokio::time::timeout(
        Duration::from_secs(3),
        changes.wait_for(|s| s.status == ResizeStatus::Applied && s.applied == Some(size(50, 120))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(received.recv().await, Some(size(50, 120)));
    // Closing the coordinator must transfer guest I/O to the next live viewer.
    let c = hub.register();
    c.report(size(60, 150));
    let successor = tokio::spawn(Arc::clone(&hub).synchronize(c.id, exited));
    worker.abort();
    drop(a);
    tokio::time::timeout(
        Duration::from_secs(3),
        changes.wait_for(|s| s.status == ResizeStatus::Applied && s.applied == Some(size(60, 150))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(received.recv().await, Some(size(60, 150)));
    exit.send(true).unwrap();
    successor.await.unwrap();
    drop(c);
    server.abort();
}

#[tokio::test]
async fn an_invalid_guest_reply_does_not_confirm_dimensions() {
    for greeting in [b"BAD\n".as_slice(), b"OK 1073741824\n".as_slice()] {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        let path = directory.path().join("v.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 14];
            stream.read_exact(&mut request).await.unwrap();
            stream.write_all(greeting).await.unwrap();
            if greeting.starts_with(b"OK ") {
                let mut frame = [0; 8];
                stream.read_exact(&mut frame).await.unwrap();
                stream.write_all(b"FCE1\x00\x00\x00\x16").await.unwrap();
            }
        });
        assert!(apply_size(path, size(40, 120)).await.is_err());
        server.await.unwrap();
    }
}
