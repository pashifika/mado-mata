#[test]
fn publication_pins_paths_and_removes_only_its_descriptor() {
    let fixture = Fixture::new();
    let (path, descriptor) = fixture.ready();
    let unrelated = fixture.collaboration_directory().join("unrelated.json");
    fs::write(&unrelated, b"unrelated").unwrap();
    assert!(fs::rename(&path, fixture.base.join("moved.json")).is_err());
    assert!(
        fs::rename(
            fixture.collaboration_directory(),
            fixture.base.join("moved")
        )
        .is_err()
    );
    assert!(fs::write(&path, b"replacement").is_err());
    assert!(
        descriptor["socket"]
            .as_str()
            .unwrap()
            .starts_with(super::super::endpoint::PIPE_PREFIX)
    );
    fixture.collaboration.shutdown(Duration::ZERO);
    assert!(!path.exists());
    assert_eq!(fs::read(unrelated).unwrap(), b"unrelated");
    assert!(LocalStream::connect(descriptor["socket"].as_str().unwrap()).is_err());
    assert_eq!(fixture.occupancy(), (0, 0));
}

#[test]
fn shutdown_joins_blocked_accept_idle_read_partial_read_and_full_write() {
    let fixture = Fixture::with(Settings {
        timeout: Duration::from_secs(30),
        write_timeout: Duration::from_millis(200),
    });
    let (_, descriptor) = fixture.ready();
    let _idle = Peer::connect(&descriptor);
    let mut partial = Peer::connect(&descriptor);
    partial.raw(&[0]);
    let mut unread = Peer::connect(&descriptor);
    unread.request(
        "large",
        Some(OWNER),
        &json!({"kind": "read", "resource": "f"}),
    );
    let ticket = fixture.ticket();
    assert!(fixture.collaboration.claim(&ticket).is_some());
    fixture.reply(&ticket, ok(&json!({"text": "x".repeat(FRAME_BYTES / 2)})));
    let started = Instant::now();
    fixture.collaboration.shutdown(Duration::ZERO);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(fixture.occupancy(), (0, 0));
    // A second shutdown neither restarts nor detaches any worker.
    fixture.collaboration.shutdown(Duration::ZERO);
}

#[test]
fn windows_endpoint_refuses_missing_root_and_non_private_existing_directory() {
    let fixture = Fixture::new();
    let missing = fixture.base.join("missing");
    let absent = Collaboration::new(Some(missing.clone()), Arc::new(|_| true), Arc::new(|| None));
    assert_eq!(
        absent.ready().unwrap_err().category,
        "CollaborationUnavailable"
    );
    assert!(!missing.exists());
    // The host never upgrades/replaces an existing unprotected directory's ACL.
    fs::create_dir(fixture.collaboration_directory()).unwrap();
    assert_eq!(
        fixture.collaboration.ready().unwrap_err().category,
        "CollaborationUnavailable"
    );
    assert_eq!(
        fs::read_dir(fixture.collaboration_directory())
            .unwrap()
            .count(),
        0
    );
}
