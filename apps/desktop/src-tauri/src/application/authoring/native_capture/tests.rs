use super::*;
use mado_runtime_comparison::authoring_capture::{CaptureGeometry, DetachedCapture};
use mado_runtime_comparison::images::{DecodedImage, PayloadBytes, encode_input};

struct PublicationFixture {
    sources: super::super::tests::Sources,
    owner: AuthoringRef,
    package: Arc<crate::authoring::Candidate>,
    source: Source,
    correlation: CaptureIdentity,
    authority: Arc<CaptureAuthority>,
    cancelled: Arc<AtomicBool>,
    document_revision: u64,
}

impl PublicationFixture {
    fn new() -> Self {
        let sources = super::super::tests::Sources::new();
        let app = sources.app();
        let workspace = app.create_workspace("publication", "Publication").unwrap();
        let editor = app
            .authoring_create(
                &crate::application::test_support::view_ref(&workspace),
                "publication",
            )
            .unwrap();
        let workspace = app.authoring_exit(&editor.owner).unwrap();
        let inspected = app
            .inspect(
                Path::new(&editor.package_path),
                &crate::application::test_support::view_ref(&workspace),
            )
            .unwrap();
        let editor = app
            .authoring_open(
                &crate::application::test_support::view_ref(&inspected.workspace),
                Path::new(&editor.package_path),
            )
            .unwrap();
        let document_revision = app
            .recognition_prepare_capture(
                &editor.owner,
                &editor.revision,
                None,
                0,
                true,
                &[],
                &std::collections::BTreeMap::new(),
            )
            .unwrap();
        let state = lock(&app.workspaces);
        let package = state
            .authoring_revision(&editor.owner, &editor.revision)
            .unwrap();
        let internal_name = state
            .resolve_workspace(&editor.owner.workspace)
            .unwrap()
            .internal_name
            .clone();
        let record = lock(&app.store)
            .read_target(&internal_name, package.package_id())
            .unwrap();
        drop(state);
        // The tests start at the already-detached worker boundary. No native call is authorized.
        let source = Source::Macos {
            internal_name,
            record,
            declaration: crate::target::tests::declaration(),
            proof: crate::target::AuthoringApplication {
                processes: Vec::new(),
                installation: "retained-installation".into(),
            },
        };
        let correlation = CaptureIdentity {
            owner: editor.owner.token.clone(),
            package_revision: editor.revision,
            binding_revision: source.binding_revision().unwrap(),
            selection_generation: 1,
            request_id: "detached-request".into(),
        };
        let authority = Arc::new(CaptureAuthority::new(correlation.clone()).unwrap());
        let fixture = Self {
            sources,
            owner: editor.owner,
            package,
            source,
            correlation,
            authority,
            cancelled: Arc::new(AtomicBool::new(false)),
            document_revision,
        };
        fixture.reserve();
        fixture
    }

    fn reserve(&self) {
        let app = self.sources.app();
        let mut state = lock(&app.workspaces);
        let native = &mut state.authoring.as_mut().unwrap().native;
        native.active = true;
        native.cancelled = self.cancelled.clone();
        lock(&app.authoring_stop).as_mut().unwrap().native = Some(NativeControl {
            generation: 1,
            cancelled: self.cancelled.clone(),
            worker: None,
            authority: Some(self.authority.clone()),
        });
    }

    fn publication(&self) -> Publication<'_> {
        Publication {
            owner: &self.owner,
            package: &self.package,
            source: &self.source,
            correlation: &self.correlation,
            authority: &self.authority,
            cancelled: &self.cancelled,
            deadline: Instant::now() + Duration::from_secs(10),
        }
    }

    fn capture(&self) -> DetachedCapture {
        let image = DecodedImage::from_rgba(1, 1, vec![12, 34, 56, 255]).unwrap();
        let (bytes, reservation) = encode_input(&image).unwrap().into_parts();
        DetachedCapture {
            identity: self.correlation.clone(),
            geometry: CaptureGeometry {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                pixels_per_unit_x: 1.0,
                pixels_per_unit_y: 1.0,
                pixel_width: 1,
                pixel_height: 1,
                unit: CoordinateUnit::MacosGlobalPoints,
            },
            acquired_at_ms: 123,
            captured_monotonic_us: 456,
            frame_identity: "retained-frame".into(),
            image,
            png: PayloadBytes::from_reserved(bytes, reservation).unwrap(),
        }
    }

    fn prepare(&self, state: &mut Workspaces) -> super::super::recognition::PreparedNativeFrame {
        self.sources
            .app()
            .prepare_native_install(
                state,
                &self.owner,
                self.package.revision(),
                None,
                self.document_revision,
                self.capture(),
                true,
            )
            .unwrap()
    }

    fn assert_unpublished(&self, state: &mut Workspaces) {
        let app = self.sources.app();
        assert!(state.authoring.as_ref().unwrap().native.operation_active());
        assert!(lock(&app.authoring_stop).as_ref().unwrap().native.is_some());
        let view = app
            .recognition_snapshot(state, &self.owner, self.package.revision())
            .unwrap();
        assert!(view.frame.is_none());
        assert!(view.capture_id.is_none());
        assert!(view.document.is_none());
        assert!(view.captures.is_empty());
        assert_eq!(view.document_revision, self.document_revision);
        state.authoring.as_mut().unwrap().native.retire("failed");
        lock(&app.authoring_stop).as_mut().unwrap().native = None;
    }
}

#[test]
fn stop_during_native_preparation_discards_pixels_and_metadata_without_waiting() {
    let fixture = PublicationFixture::new();
    let app = fixture.sources.app();
    let command = lock(&app.commands);
    let mut state = lock(&app.workspaces);
    let mut stop_result = None;
    let mut stop_thread = None;
    let result = app.publish_native_frame(&mut state, fixture.publication(), |state| {
        let prepared = fixture.prepare(state);
        let store = lock(&app.store);
        let stopper = app.clone();
        let owner = fixture.owner.clone();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        stop_thread = Some(std::thread::spawn(move || {
            let _ = send.send(stopper.authoring_stop(&owner));
        }));
        stop_result = Some(receive.recv_timeout(Duration::from_secs(1)));
        drop(store);
        Ok(prepared)
    });
    stop_thread.unwrap().join().unwrap();
    assert!(stop_result.unwrap().unwrap().unwrap());
    assert_eq!(result.unwrap_err().category, "Cancelled");
    assert_eq!(
        fixture
            .authority
            .check(&fixture.correlation)
            .unwrap_err()
            .category,
        "Cancelled"
    );
    fixture.assert_unpublished(&mut state);
    drop(state);
    drop(command);
}

#[test]
fn stop_after_native_publication_preserves_committed_historical_pixels() {
    let fixture = PublicationFixture::new();
    let app = fixture.sources.app();
    let mut state = lock(&app.workspaces);
    let png = app
        .publish_native_frame(&mut state, fixture.publication(), |state| {
            Ok(fixture.prepare(state))
        })
        .unwrap();
    let before = app
        .recognition_snapshot(&mut state, &fixture.owner, fixture.package.revision())
        .unwrap();
    assert!(app.authoring_stop(&fixture.owner).unwrap());
    let after = app
        .recognition_snapshot(&mut state, &fixture.owner, fixture.package.revision())
        .unwrap();
    let frame = after.frame.unwrap();
    assert_eq!(frame.id, before.frame.unwrap().id);
    assert_eq!(frame.historical_capture_at_ms, Some(123));
    assert_eq!(after.capture_id, before.capture_id);
    assert_eq!(after.captures, before.captures);
    let decoded = mado_runtime_comparison::images::decode_png(
        png.as_slice(),
        mado_runtime_comparison::images::ImageKind::Input,
    )
    .unwrap();
    assert_eq!(decoded.rgba, [12, 34, 56, 255]);
    state.authoring.as_mut().unwrap().native.retire("cancelled");
    lock(&app.authoring_stop).as_mut().unwrap().native = None;
}

#[test]
fn changed_package_during_detached_preparation_discards_pixels_and_metadata() {
    let fixture = PublicationFixture::new();
    let app = fixture.sources.app();
    let mut state = lock(&app.workspaces);
    let result = app.publish_native_frame(&mut state, fixture.publication(), |state| {
        let prepared = fixture.prepare(state);
        let path = fixture.package.root().join("main.ts");
        let mut source = std::fs::read_to_string(&path).unwrap();
        source.push_str("\n// External package edit during detached image preparation.\n");
        std::fs::write(path, source).unwrap();
        Ok(prepared)
    });
    assert_eq!(result.unwrap_err().category, "StaleNativeSelection");
    fixture.assert_unpublished(&mut state);
}

#[cfg(unix)]
#[test]
fn changed_saved_binding_during_detached_preparation_discards_pixels_and_metadata() {
    let metadata = crate::target::tests::MetadataFixture::new();
    let executable = metadata.executable("game");
    let mut fixture = PublicationFixture::new();
    if let Source::Macos {
        internal_name,
        record,
        declaration,
        ..
    } = &mut fixture.source
    {
        // A real saved-record transition; the fixture executable is never launched.
        *record = lock(&fixture.sources.app().store)
            .save_target(
                internal_name,
                fixture.package.package_id(),
                declaration,
                &record.expectation(),
                crate::target::tests::configuration(executable.to_str().unwrap()),
                None,
            )
            .unwrap()
            .0;
    }
    fixture.correlation.binding_revision = fixture.source.binding_revision().unwrap();
    fixture.authority = Arc::new(CaptureAuthority::new(fixture.correlation.clone()).unwrap());
    fixture.reserve();
    let app = fixture.sources.app();
    let mut state = lock(&app.workspaces);
    let result = app.publish_native_frame(&mut state, fixture.publication(), |state| {
        let prepared = fixture.prepare(state);
        let Source::Macos {
            internal_name,
            record,
            ..
        } = &fixture.source
        else {
            unreachable!()
        };
        lock(&app.store)
            .remove_target(
                internal_name,
                fixture.package.package_id(),
                &record.expectation(),
            )
            .unwrap();
        Ok(prepared)
    });
    assert_eq!(result.unwrap_err().category, "StaleNativeSelection");
    fixture.assert_unpublished(&mut state);
}

#[cfg(unix)]
#[test]
fn reset_target_preserves_detached_recognition_and_refuses_incomplete_cleanup() {
    let metadata = crate::target::tests::MetadataFixture::new();
    let executable = metadata.executable("game");
    let fixture = PublicationFixture::new();
    let app = fixture.sources.app();
    let mut state = lock(&app.workspaces);
    app.publish_native_frame(&mut state, fixture.publication(), |state| {
        Ok(fixture.prepare(state))
    })
    .unwrap();
    state.authoring.as_mut().unwrap().native.active = false;
    lock(&app.authoring_stop).as_mut().unwrap().native = None;
    let before = app
        .recognition_snapshot(&mut state, &fixture.owner, fixture.package.revision())
        .unwrap();
    drop(state);
    let Source::Macos {
        internal_name,
        record,
        declaration,
        ..
    } = &fixture.source
    else {
        unreachable!()
    };
    let mut configuration = crate::target::tests::configuration(executable.to_str().unwrap());
    configuration.input = None;
    let saved = lock(&app.store)
        .save_target(
            internal_name,
            fixture.package.package_id(),
            declaration,
            &record.expectation(),
            configuration,
            None,
        )
        .unwrap()
        .0;
    let manifest = std::fs::read(fixture.package.root().join("package.json")).unwrap();
    {
        let mut state = lock(&app.workspaces);
        state.authoring.as_mut().unwrap().containment = Some(Fault::new(
            "NativeCaptureCleanup",
            "test boundary: child is not reaped",
        ));
    }
    assert_eq!(
        app.native_reset_target(&fixture.owner, fixture.package.revision())
            .unwrap_err()
            .category,
        "NativeCaptureCleanup"
    );
    assert_eq!(
        lock(&app.store)
            .read_target(internal_name, fixture.package.package_id())
            .unwrap(),
        saved
    );
    lock(&app.workspaces)
        .authoring
        .as_mut()
        .unwrap()
        .containment = None;
    let selection = app
        .native_reset_target(&fixture.owner, fixture.package.revision())
        .unwrap();
    assert!(!selection.has_saved_target);
    assert!(!selection.occupied);
    assert!(
        lock(&app.store)
            .read_target(internal_name, fixture.package.package_id())
            .unwrap()
            .binding
            .is_none()
    );
    let after = app
        .recognition_view(&fixture.owner, fixture.package.revision())
        .unwrap();
    assert_eq!(after.capture_id, before.capture_id);
    assert_eq!(
        after.frame.as_ref().unwrap().id,
        before.frame.as_ref().unwrap().id
    );
    assert_eq!(after.document_revision, before.document_revision);
    assert_eq!(
        serde_json::to_value(&after.document).unwrap(),
        serde_json::to_value(&before.document).unwrap()
    );
    assert_eq!(
        std::fs::read(fixture.package.root().join("package.json")).unwrap(),
        manifest
    );
}

#[cfg(unix)]
#[test]
fn package_revision_rebase_requires_the_same_target_declaration_and_binding() {
    let metadata = crate::target::tests::MetadataFixture::new();
    let executable = metadata.executable("game");
    let mut fixture = PublicationFixture::new();
    let declaration = crate::target::tests::declaration();
    let Source::Macos {
        internal_name,
        record,
        ..
    } = &mut fixture.source
    else {
        unreachable!()
    };
    *record = lock(&fixture.sources.app().store)
        .save_target(
            internal_name,
            fixture.package.package_id(),
            &declaration,
            &record.expectation(),
            crate::target::tests::configuration(executable.to_str().unwrap()),
            None,
        )
        .unwrap()
        .0;
    fixture
        .source
        .check_target(fixture.package.package_id(), Some(&declaration))
        .unwrap();
    let mut changed = declaration.clone();
    changed.window_title = Some("Another target window".into());
    assert_eq!(
        fixture
            .source
            .check_target(fixture.package.package_id(), Some(&changed))
            .unwrap_err()
            .category,
        "StaleNativeSelection"
    );
    assert_eq!(
        fixture
            .source
            .check_target(fixture.package.package_id(), None)
            .unwrap_err()
            .category,
        "StaleNativeSelection"
    );
    assert_eq!(
        fixture
            .source
            .check_target("another-package", Some(&declaration))
            .unwrap_err()
            .category,
        "StaleNativeSelection"
    );
    lock(&fixture.sources.app().workspaces)
        .authoring
        .as_mut()
        .unwrap()
        .native
        .active = false;
    lock(&fixture.sources.app().authoring_stop)
        .as_mut()
        .unwrap()
        .native = None;
}
