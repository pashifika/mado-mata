use super::*;
use catalog::Asset;
use mado_runtime_comparison::environment::{BOUNDED_PROFILE, G004_PROFILE};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::sync::atomic::AtomicU64;
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);
const CONTENT: &[u8] = b"deterministic model transfer fixture";
const THIRD_PROFILE: &str = "ppocrv6-tiny-det-small-rec-max736-v1";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mado-ocr-setup-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn relative() -> Self {
        let path = PathBuf::from(format!(
            "mado-ocr-relative-{}",
            crate::storage::new_id().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn installed(&self, catalog: &Catalog) -> PathBuf {
        self.0
            .join("ocr-resources")
            .join(catalog.installation_name())
    }
    fn staging(&self) -> PathBuf {
        self.0.join("ocr-resources/staging-v1")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture_catalog() -> Catalog {
    fixture_profile(DEFAULT_OCR_PROFILE_ID)
}

fn fixture_profile(profile: &str) -> Catalog {
    let mut catalog = Catalog::for_profile(profile).unwrap();
    for asset in &mut catalog.models.assets {
        asset.bytes = CONTENT.len() as u64;
        asset.sha256 = format!("{:x}", Sha256::digest(CONTENT));
    }
    catalog
}

fn fetch_fixture(
    asset: &Asset,
    file: &mut File,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(u64),
) -> Result<(), Fault> {
    download::transfer(&mut Cursor::new(CONTENT), file, asset, cancel, report)
}

fn install(fixture: &Fixture, catalog: &Catalog) {
    download::acquire_with(
        &fixture.0,
        catalog,
        &AtomicBool::new(false),
        |_| {},
        fetch_fixture,
    )
    .unwrap();
}

#[test]
fn guidance_rejects_caller_selected_resources() {
    assert!(guidance_link("https://caller.example/install", 0).is_err());
    assert!(guidance_command("brew install caller-selected-package", 0).is_err());
}

#[test]
fn catalog_presentation_changes_do_not_create_new_model_installations() {
    let original = Catalog::load().unwrap();
    let mut changed = original.clone();
    changed.platforms[0].native.commands = vec!["brew info opencv@4".into()];
    changed.models.name.en = "Updated model title".into();
    changed.validate().unwrap();
    assert_eq!(changed.installation_name(), original.installation_name());
    assert_eq!(changed.items()[0].name.en, "Updated model title");
    changed.models.assets[0].url = "https://example.com/unreviewed-model.onnx".into();
    assert!(changed.validate().is_err());
}

#[test]
fn catalog_refuses_unsupported_content_and_dependency_mapping() {
    let original = Catalog::load().unwrap();
    let mut catalog = original.clone();
    catalog.models.assets[0].sha256 = "0".repeat(64);
    assert!(catalog.validate().is_err());
    catalog = original.clone();
    catalog.models.assets[1] = catalog.models.assets[0].clone();
    assert!(catalog.validate().is_err());
    assert!(Catalog::for_profile("arbitrary-profile").is_err());
    catalog = original.clone();
    catalog.platforms[0].arch = "x86_64".into();
    assert!(catalog.validate().is_err());
    catalog = original.clone();
    catalog.platforms[0].native.required_modules.clear();
    assert!(catalog.validate().is_err());
    catalog = original;
    catalog.models.assets[0].url.clear();
    assert!(catalog.validate().is_err());
}

#[test]
fn catalog_rejects_unsafe_destinations_and_non_https_sources() {
    for path in [
        "../model",
        "/model",
        "C:/model",
        "dir\\model",
        "a//model",
        "a/./model",
        "model\n",
    ] {
        assert!(catalog::safe_relative(path).is_err(), "{path}");
    }
    for url in [
        "http://example.com/model",
        "https://user:password@example.com/model",
        "file:///model",
    ] {
        let mut catalog = Catalog::load().unwrap();
        catalog.models.assets[0].url = url.into();
        assert!(catalog.validate().is_err(), "{url}");
    }
}

#[test]
fn partial_hints_keep_supported_profile_and_reject_nonblank_unsupported_tuple() {
    let mut hints = proposed_tuple(BOUNDED_PROFILE, None).unwrap();
    hints.model_root = "/missing-models".into();
    assert_eq!(
        proposed_tuple(BOUNDED_PROFILE, Some(&hints)).unwrap().profile,
        BOUNDED_PROFILE
    );
    hints.provider = "cuda".into();
    assert!(proposed_tuple(BOUNDED_PROFILE, Some(&hints)).is_err());
    hints.provider.clear();
    hints.profile = "custom-model".into();
    assert!(proposed_tuple(BOUNDED_PROFILE, Some(&hints)).is_err());
    hints.profile.clear();
    assert_eq!(proposed_tuple(BOUNDED_PROFILE, Some(&hints)).unwrap().profile, BOUNDED_PROFILE);
    assert!(proposed_tuple("", None).is_err());
    assert!(proposed_tuple("custom-model", None).is_err());
}

#[test]
fn choices_preserve_failed_rules_and_their_target_scoped_evidence() {
    let view = catalog_view().unwrap();
    assert_eq!(view.default_profile, G004_PROFILE);
    assert_eq!(view.profile, G004_PROFILE);
    assert_eq!(view.target, OCR_TARGET);
    for choice in &view.profiles {
        assert!(view.items.iter().any(|item| item.id == choice.resource_id));
        validate_request(choice.profile, &choice.resource_id, None).unwrap();
    }
    let third = view
        .profiles
        .iter()
        .find(|choice| choice.profile == THIRD_PROFILE)
        .unwrap();
    let summary = |target| {
        third.evaluations.iter().find(|entry| entry.target == target).unwrap()
    };
    let apple = summary("aarch64-apple-darwin");
    let windows = summary("x86_64-pc-windows-msvc");
    assert_eq!(apple.provider, PROVIDER);
    assert_eq!(windows.provider, PROVIDER);
    assert_eq!(apple.rules.len(), 13);
    assert_eq!(windows.rules.len(), 13);
    assert!(
        apple.rules.iter().any(|rule| rule.rule_id == "text_order" && rule.outcome == "failed")
    );
    assert!(apple.rules.iter().any(|rule| {
        rule.rule_id == "unexpected_detection" && rule.outcome == "passed"
    }));
    assert!(windows.rules.iter().any(|rule| {
        rule.rule_id == "unexpected_detection" && rule.outcome == "failed"
    }));
    for scope in [apple, windows] {
        let evidence = scope.evidence.as_ref().unwrap();
        assert_eq!(scope.criteria, "tools/ocr-models/criteria-v1.json");
        assert_eq!(
            evidence.report,
            "docs/benchmarks/ocr-models/cf2763a-evaluation-v1.json"
        );
        assert_eq!(
            evidence.measured_source_revision,
            "cf2763a41929ad89366dd77ee1b6a57503a8f6bc"
        );
        assert_eq!(
            evidence.execution_sha256,
            digest_hex(&ocr_profile(THIRD_PROFILE).unwrap().execution_sha256)
        );
        assert!(!evidence.evaluator_revision.is_empty());
    }
    let encoded = serde_json::to_value(third).unwrap();
    assert_eq!(encoded["evaluations"][0]["rules"][0]["outcome"], "failed");
    assert_eq!(
        encoded["evaluations"][0]["evidence"]["execution_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
}

#[test]
fn released_profiles_share_the_original_installation_and_previous_receipt() {
    let legacy = Catalog::for_profile(G004_PROFILE).unwrap();
    let bounded = Catalog::for_profile(BOUNDED_PROFILE).unwrap();
    assert_eq!(legacy.models.id, "rapidocr-models");
    assert_eq!(bounded.models.id, legacy.models.id);
    assert_eq!(
        legacy.installation_name(),
        "models-f0f4a0cd3d9469fa91038612e9da6813f7e2bb82bc0ba29cc7894280d019c4cb"
    );
    assert_eq!(bounded.installation_name(), legacy.installation_name());
    assert_ne!(
        Catalog::for_profile(THIRD_PROFILE).unwrap().installation_name(),
        legacy.installation_name()
    );

    let fixture = Fixture::new();
    let mut previous = fixture_profile(G004_PROFILE);
    previous.engine_revision = "453132bcf4128c180dc0699dfccca5fe1f098164".into();
    install(&fixture, &previous);
    let path = fixture.installed(&previous).join("receipt.json");
    let receipt = fs::read(&path).unwrap();
    let bounded = fixture_profile(BOUNDED_PROFILE);
    download::acquire_with(
        &fixture.0,
        &bounded,
        &AtomicBool::new(false),
        |_| {},
        |_, _, _, _| panic!("shared models must not be fetched again"),
    )
    .unwrap();
    assert_eq!(fs::read(path).unwrap(), receipt);
    let view = inspect_with(&bounded, &fixture.0, None, None, &AtomicBool::new(false)).unwrap();
    assert_eq!(view.profile, BOUNDED_PROFILE);
    assert_eq!(view.resource_id, "rapidocr-models");
    assert_eq!(
        view.resolved.model_root.as_deref(),
        fixture.installed(&bounded).to_str()
    );
}

#[test]
fn selected_profile_and_resource_must_agree_before_any_filesystem_action() {
    let fixture = Fixture::new();
    let third = Catalog::for_profile(THIRD_PROFILE).unwrap();
    for (profile, resource) in [
        ("", "rapidocr-models"),
        ("unknown", "rapidocr-models"),
        (THIRD_PROFILE, "rapidocr-models"),
        (G004_PROFILE, third.models.id.as_str()),
    ] {
        assert!(
            download(&fixture.0, profile, resource, &AtomicBool::new(false), |_| {}).is_err()
        );
        assert!(!fixture.0.join("ocr-resources").exists());
    }
    let hints = proposed_tuple(G004_PROFILE, None).unwrap();
    assert!(
        inspect(&fixture.0, THIRD_PROFILE, Some(&hints), None, &AtomicBool::new(false)).is_err()
    );
    assert!(!fixture.0.join("ocr-resources").exists());
}

#[test]
fn auxiliary_failure_never_publishes_a_partial_set_or_changes_an_existing_set() {
    let fixture = Fixture::new();
    let legacy = fixture_catalog();
    install(&fixture, &legacy);
    let receipt = fs::read(fixture.installed(&legacy).join("receipt.json")).unwrap();
    let third = fixture_profile(THIRD_PROFILE);
    let auxiliary = third.profile.auxiliary.unwrap().relative_path;
    let mut fetched = 0;
    let error = download::acquire_with(
        &fixture.0,
        &third,
        &AtomicBool::new(false),
        |_| {},
        |asset, output, cancel, report| {
            fetched += 1;
            if asset.path == auxiliary {
                return Err(fault("download", "vocabulary transfer failed"));
            }
            fetch_fixture(asset, output, cancel, report)
        },
    )
    .unwrap_err();
    assert_eq!(fetched, 3);
    assert_eq!(error.message, "vocabulary transfer failed");
    assert!(!fixture.installed(&third).exists());
    assert!(!fixture.staging().exists());
    assert_eq!(
        fs::read(fixture.installed(&legacy).join("receipt.json")).unwrap(),
        receipt
    );
    files::models(&fixture.installed(&legacy), &legacy, &AtomicBool::new(false)).unwrap();
}

#[test]
fn missing_or_tampered_auxiliary_refuses_reuse_and_manual_inspection_without_replacement() {
    for missing in [true, false] {
        let fixture = Fixture::new();
        let catalog = fixture_profile(THIRD_PROFILE);
        install(&fixture, &catalog);
        let installed = fixture.installed(&catalog);
        let sidecar = installed.join(catalog.profile.auxiliary.unwrap().relative_path);
        let receipt = fs::read(installed.join("receipt.json")).unwrap();
        if missing {
            fs::remove_file(&sidecar).unwrap();
        } else {
            fs::write(&sidecar, vec![b'x'; CONTENT.len()]).unwrap();
        }
        let view = inspect_with(&catalog, &fixture.0, None, None, &AtomicBool::new(false)).unwrap();
        assert_eq!(view.profile, THIRD_PROFILE);
        assert_eq!(view.resource_id, catalog.models.id);
        assert_eq!(
            view.items[0].state,
            if missing { "missing" } else { "incompatible" }
        );
        assert!(view.resolved.model_root.is_none());
        assert!(view.environment.is_none());
        assert!(
            download::acquire_with(
                &fixture.0,
                &catalog,
                &AtomicBool::new(false),
                |_| {},
                |_, _, _, _| panic!("invalid published resources must never be replaced"),
            )
            .is_err()
        );
        let mut hints = proposed_tuple(THIRD_PROFILE, None).unwrap();
        hints.model_root = installed.to_str().unwrap().into();
        let manual = inspect_with(
            &catalog,
            &fixture.0,
            Some(&hints),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(manual.resolved.model_root.is_none());
        assert_eq!(fs::read(installed.join("receipt.json")).unwrap(), receipt);
        if missing {
            assert!(!sidecar.exists());
        } else {
            assert_eq!(fs::read(sidecar).unwrap(), vec![b'x'; CONTENT.len()]);
        }
    }
}

#[test]
fn changed_staged_auxiliary_is_reverified_before_complete_set_publication() {
    let fixture = Fixture::new();
    let catalog = fixture_profile(THIRD_PROFILE);
    let sidecar = fixture
        .staging()
        .join(catalog.profile.auxiliary.unwrap().relative_path);
    let error = download::acquire_with(
        &fixture.0,
        &catalog,
        &AtomicBool::new(false),
        |progress| {
            if progress.stage == "verifying" {
                fs::write(&sidecar, vec![b'x'; CONTENT.len()]).unwrap();
            }
        },
        fetch_fixture,
    )
    .unwrap_err();
    assert_eq!(error.context["stage"], "integrity");
    assert!(!fixture.installed(&catalog).exists());
    assert!(!fixture.staging().exists());
}

#[test]
fn explicit_profile_switch_recovers_only_known_interrupted_resources() {
    let fixture = Fixture::new();
    let legacy = fixture_catalog();
    install(&fixture, &legacy);
    let third = Catalog::for_profile(THIRD_PROFILE).unwrap();
    let sidecar = fixture
        .staging()
        .join(third.profile.auxiliary.unwrap().relative_path);
    fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    fs::write(sidecar, b"incomplete vocabulary").unwrap();
    drop(download::recover(&fixture.0, &legacy).unwrap());
    assert!(!fixture.staging().exists());
    files::models(&fixture.installed(&legacy), &legacy, &AtomicBool::new(false)).unwrap();
}

#[cfg(unix)]
#[test]
fn exact_auxiliary_bytes_outside_the_model_root_are_not_adopted_through_a_symlink() {
    let fixture = Fixture::new();
    let catalog = fixture_profile(THIRD_PROFILE);
    install(&fixture, &catalog);
    let installed = fixture.installed(&catalog);
    let sidecar = installed.join(catalog.profile.auxiliary.unwrap().relative_path);
    let external = fixture.0.join("external-vocabulary.yml");
    fs::write(&external, CONTENT).unwrap();
    fs::remove_file(&sidecar).unwrap();
    std::os::unix::fs::symlink(&external, &sidecar).unwrap();
    let mut hints = proposed_tuple(THIRD_PROFILE, None).unwrap();
    hints.model_root = installed.to_str().unwrap().into();
    let view = inspect_with(
        &catalog,
        &fixture.0,
        Some(&hints),
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(view.resolved.model_root.is_none());
    assert_eq!(view.items[0].state, "incompatible");
    assert_eq!(fs::read(external).unwrap(), CONTENT);
    assert!(fs::symlink_metadata(sidecar).unwrap().file_type().is_symlink());
}

#[test]
fn absent_resources_do_not_create_directories_or_propose_environment() {
    for fixture in [Fixture::new(), Fixture::relative()] {
        let view = inspect(&fixture.0, DEFAULT_OCR_PROFILE_ID, None, None, &AtomicBool::new(false)).unwrap();
        assert!(view.environment.is_none());
        assert_eq!(view.items[0].state, "missing");
        assert!(!fixture.0.join("ocr-resources").exists());
        let error = download(
            &fixture.0,
            DEFAULT_OCR_PROFILE_ID,
            "https://caller.example/model",
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.context["stage"], "resource");
        assert!(!fixture.0.join("ocr-resources").exists());
    }
}

#[test]
fn truncated_wrong_and_oversized_transfers_never_publish_or_change_settings() {
    for bytes in [
        &CONTENT[..CONTENT.len() - 1],
        &[b'x'; CONTENT.len()][..],
        &[b'x'; 80][..],
    ] {
        let fixture = Fixture::new();
        let catalog = fixture_catalog();
        fs::write(fixture.0.join("settings.json"), "saved settings").unwrap();
        let error = download::acquire_with(
            &fixture.0,
            &catalog,
            &AtomicBool::new(false),
            |_| {},
            |asset, file, cancel, report| {
                download::transfer(&mut Cursor::new(bytes), file, asset, cancel, report)
            },
        )
        .unwrap_err();
        assert_eq!(error.context["stage"], "integrity");
        assert!(!fixture.installed(&catalog).exists());
        assert!(!fixture.staging().exists());
        assert_eq!(
            fs::read_to_string(fixture.0.join("settings.json")).unwrap(),
            "saved settings"
        );
    }
}

#[test]
fn interrupted_network_and_write_errors_remain_primary_and_cleanup_staging() {
    struct BrokenRead;
    impl Read for BrokenRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("connection interrupted"))
        }
    }
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    let error = download::acquire_with(
        &fixture.0,
        &catalog,
        &AtomicBool::new(false),
        |_| {},
        |asset, file, cancel, report| {
            download::transfer(&mut BrokenRead, file, asset, cancel, report)
        },
    )
    .unwrap_err();
    assert!(error.message.contains("connection interrupted"));
    assert!(!fixture.staging().exists());
    struct BrokenWrite;
    impl Write for BrokenWrite {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let error = download::transfer(
        &mut Cursor::new(CONTENT),
        &mut BrokenWrite,
        &catalog.models.assets[0],
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(error.message.contains("disk full"));
}

#[test]
fn cancellation_before_publication_removes_staging_but_after_commit_preserves_installation() {
    for (profile, stage) in [G004_PROFILE, THIRD_PROFILE].into_iter().flat_map(|profile| {
        ["downloading", "publishing", "complete"].map(|stage| (profile, stage))
    }) {
        let fixture = Fixture::new();
        let catalog = fixture_profile(profile);
        let cancel = AtomicBool::new(false);
        let result = download::acquire_with(
            &fixture.0,
            &catalog,
            &cancel,
            |progress| {
                if progress.stage == stage {
                    cancel.store(true, Ordering::Release);
                }
            },
            fetch_fixture,
        );
        if stage == "complete" {
            result.unwrap();
            assert!(fixture.installed(&catalog).is_dir());
            let view = inspect_with(&catalog, &fixture.0, None, None, &AtomicBool::new(false)).unwrap();
            assert_eq!(view.profile, profile);
            assert_eq!(view.resource_id, catalog.models.id);
            assert_eq!(view.resolved.model_root.as_deref(), fixture.installed(&catalog).to_str());
        } else {
            assert_eq!(result.unwrap_err().category, "OcrSetupCancelled");
            assert!(!fixture.installed(&catalog).exists());
        }
        assert!(!fixture.staging().exists());
    }
}

#[test]
fn verified_installation_is_reused_without_fetch_and_corrupt_one_is_not_overwritten() {
    for fixture in [Fixture::new(), Fixture::relative()] {
        let catalog = fixture_catalog();
        let mut previous = catalog.clone();
        previous.engine_revision = "1".repeat(40);
        install(&fixture, &previous);
        let receipt_path = fixture.installed(&catalog).join("receipt.json");
        let receipt = fs::read(&receipt_path).unwrap();
        let view = inspect_with(&catalog, &fixture.0, None, None, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            view.resolved.model_root,
            Some(
                fixture
                    .installed(&catalog)
                    .canonicalize()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            )
        );
        download::verify_receipt(&fixture.installed(&catalog), &catalog).unwrap();
        download::acquire_with(
            &fixture.0,
            &catalog,
            &AtomicBool::new(false),
            |_| {},
            |_, _, _, _| panic!("must reuse verified files"),
        )
        .unwrap();
        assert_eq!(fs::read(receipt_path).unwrap(), receipt);
        let model = fixture
            .installed(&catalog)
            .join(&catalog.models.assets[0].path);
        fs::write(&model, "user replacement").unwrap();
        assert!(
            download::acquire_with(
                &fixture.0,
                &catalog,
                &AtomicBool::new(false),
                |_| {},
                |_, _, _, _| panic!("must not overwrite publication")
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(model).unwrap(), "user replacement");
    }
}

#[test]
fn failed_publication_preserves_conflicting_destination_and_cleans_staging() {
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    let destination = fixture.installed(&catalog);
    let error = download::acquire_with(
        &fixture.0,
        &catalog,
        &AtomicBool::new(false),
        |progress| {
            if progress.stage == "publishing" {
                fs::write(&destination, "do not replace").unwrap();
            }
        },
        fetch_fixture,
    )
    .unwrap_err();
    assert_eq!(error.context["stage"], "publication");
    assert_eq!(fs::read_to_string(destination).unwrap(), "do not replace");
    assert!(!fixture.staging().exists());
}

#[test]
fn interrupted_owned_stage_is_reclaimed_on_explicit_work_only() {
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    install(&fixture, &catalog);
    let staged_file = fixture.staging().join(&catalog.models.assets[0].path);
    fs::create_dir_all(staged_file.parent().unwrap()).unwrap();
    fs::write(&staged_file, "incomplete").unwrap();
    catalog_view().unwrap();
    assert!(staged_file.exists());
    let lease = download::recover(&fixture.0, &catalog).unwrap();
    assert!(!fixture.staging().exists());
    assert!(fixture.installed(&catalog).exists());
    drop(lease);
}

#[test]
fn unknown_staging_files_and_cleanup_failure_are_retained_and_reported_separately() {
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    let error = download::acquire_with(
        &fixture.0,
        &catalog,
        &AtomicBool::new(false),
        |_| {},
        |_, _, _, _| {
            fs::write(fixture.staging().join("user-owned.txt"), "preserve").unwrap();
            Err(fault("download", "primary network failure"))
        },
    )
    .unwrap_err();
    assert_eq!(error.message, "primary network failure");
    let cleanup: Fault = serde_json::from_value(error.context["cleanup_error"].clone()).unwrap();
    assert_eq!(cleanup.context["stage"], "cleanup");
    assert_eq!(
        fs::read_to_string(fixture.staging().join("user-owned.txt")).unwrap(),
        "preserve"
    );
    assert!(
        download::recover(&fixture.0, &catalog).unwrap_err().context["cleanup_error"].is_object()
    );
}

#[test]
fn unowned_directory_is_never_adopted_or_deleted() {
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    fs::create_dir(fixture.0.join("ocr-resources")).unwrap();
    fs::write(fixture.0.join("ocr-resources/user.txt"), "preserve").unwrap();
    assert!(
        download::acquire_with(
            &fixture.0,
            &catalog,
            &AtomicBool::new(false),
            |_| {},
            fetch_fixture
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("ocr-resources/user.txt")).unwrap(),
        "preserve"
    );
}

#[test]
fn second_resource_owner_cannot_reclaim_an_active_stage() {
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    install(&fixture, &catalog);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.0.join("ocr-resources/.setup.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    fs::create_dir(fixture.staging()).unwrap();
    let staged = fixture.staging().join("receipt.json");
    fs::write(&staged, "active transfer").unwrap();
    let error = download::recover(&fixture.0, &catalog).unwrap_err();
    assert_eq!(error.category, "OcrSetupBusy");
    assert_eq!(fs::read_to_string(staged).unwrap(), "active transfer");
    drop(lock);
    drop(download::recover(&fixture.0, &catalog).unwrap());
    assert!(!fixture.staging().exists());
}

#[cfg(unix)]
#[test]
fn staging_and_managed_symlinks_never_delete_external_files() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    install(&fixture, &catalog);
    let external = fixture.0.join("user-data");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("receipt.json"), "user data").unwrap();
    symlink(&external, fixture.staging()).unwrap();
    assert!(download::recover(&fixture.0, &catalog).is_err());
    assert_eq!(
        fs::read_to_string(external.join("receipt.json")).unwrap(),
        "user data"
    );
    let other = Fixture::new();
    symlink(&external, other.0.join("ocr-resources")).unwrap();
    assert!(
        download::acquire_with(
            &other.0,
            &catalog,
            &AtomicBool::new(false),
            |_| {},
            fetch_fixture
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(external.join("receipt.json")).unwrap(),
        "user data"
    );
}

fn native_header(os: &str) -> Vec<u8> {
    let mut bytes = vec![0_u8; 128];
    if os == "macos" {
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        bytes[4..8].copy_from_slice(&0x0100_000c_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&6_u32.to_le_bytes());
    } else {
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64_u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&[0x64, 0x86]);
        bytes[86..88].copy_from_slice(&0x2000_u16.to_le_bytes());
    }
    bytes
}

#[test]
fn native_recheck_refuses_missing_modules_and_wrong_cpu_without_loading() {
    let catalog = Catalog::load().unwrap();
    for platform in &catalog.platforms {
        let fixture = Fixture::new();
        let mut paths = Vec::new();
        for module in &platform.native.required_modules {
            let name = if platform.os == "macos" {
                format!("{module}.14.0.dylib")
            } else {
                module.clone()
            };
            let path = fixture.0.join(name);
            fs::write(&path, native_header(&platform.os)).unwrap();
            paths.push(path.to_str().unwrap().to_owned());
        }
        assert_eq!(
            files::native(&paths, platform, &AtomicBool::new(false)).unwrap(),
            paths
        );
        assert!(files::native(&paths[1..], platform, &AtomicBool::new(false)).is_err());
        fs::write(
            &paths[0],
            native_header(if platform.os == "macos" {
                "windows"
            } else {
                "macos"
            }),
        )
        .unwrap();
        assert!(files::native(&paths, platform, &AtomicBool::new(false)).is_err());
    }
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "windows", target_arch = "x86_64")
))]
#[test]
fn recheck_returns_complete_draft_only_after_all_resources_resolve() {
    let fixture = Fixture::new();
    let mut catalog = fixture_profile(BOUNDED_PROFILE);
    install(&fixture, &catalog);
    let platform = catalog
        .platforms
        .iter_mut()
        .find(|platform| platform.os == std::env::consts::OS)
        .unwrap();
    let runtime = fixture.0.join(&platform.runtime.filename);
    let bytes = native_header(&platform.os);
    fs::write(&runtime, &bytes).unwrap();
    platform.runtime.bytes = bytes.len() as u64;
    platform.runtime.sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut hints = proposed_tuple(BOUNDED_PROFILE, None).unwrap();
    hints.runtime_path = runtime.to_str().unwrap().into();
    for module in &platform.native.required_modules {
        let name = if platform.os == "macos" {
            format!("{module}.14.0.dylib")
        } else {
            module.clone()
        };
        let path = fixture.0.join(name);
        fs::write(&path, native_header(&platform.os)).unwrap();
        hints
            .native_library_paths
            .push(path.to_str().unwrap().into());
    }
    let result = inspect_with(
        &catalog,
        &fixture.0,
        Some(&hints),
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    let proposal = result.environment.unwrap();
    assert_eq!(proposal.profile, BOUNDED_PROFILE);
    assert_eq!(
        proposal.model_root,
        fixture.installed(&catalog).to_str().unwrap()
    );
    assert!(hints.model_root.is_empty());
    fs::write(runtime, "wrong runtime").unwrap();
    let result = inspect_with(
        &catalog,
        &fixture.0,
        Some(&hints),
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(result.environment.is_none());
    assert_eq!(result.items[0].state, "verified");
    assert_eq!(result.items.iter().find(|item| item.id == "onnxruntime").unwrap().state, "incompatible");
    assert_eq!(result.items.iter().find(|item| item.id == "native-libraries").unwrap().state, "verified");
}

#[test]
fn cancellation_before_work_has_no_effect_and_does_not_mask_primary_errors() {
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    let cancel = AtomicBool::new(true);
    let error =
        download::acquire_with(&fixture.0, &catalog, &cancel, |_| {}, fetch_fixture).unwrap_err();
    assert_eq!(error.category, "OcrSetupCancelled");
    assert!(!fixture.0.join("ocr-resources").exists());
    cancel.store(false, Ordering::Release);
    let error = download::acquire_with(
        &fixture.0,
        &catalog,
        &cancel,
        |_| {},
        |_, _, _, _| {
            cancel.store(true, Ordering::Release);
            Err(fault("download", "real transfer failure"))
        },
    )
    .unwrap_err();
    assert_eq!(error.message, "real transfer failure");
    assert_eq!(error.context["stage"], "download");
    assert!(!fixture.staging().exists());
}

#[test]
fn malformed_receipt_cannot_certify_or_replace_published_files() {
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    install(&fixture, &catalog);
    let installed = fixture.installed(&catalog);
    let receipt_path = installed.join("receipt.json");
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    receipt["engine_revision"] = "z".repeat(40).into();
    for invalid in [
        "{\"schema\":".to_owned(),
        serde_json::to_string(&receipt).unwrap(),
    ] {
        fs::write(&receipt_path, &invalid).unwrap();
        assert!(download::verify_receipt(&installed, &catalog).is_err());
        assert!(
            download::acquire_with(
                &fixture.0,
                &catalog,
                &AtomicBool::new(false),
                |_| {},
                |_, _, _, _| panic!("must not replace publication")
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&receipt_path).unwrap(), invalid);
        assert_eq!(
            fs::read(installed.join(&catalog.models.assets[0].path)).unwrap(),
            CONTENT
        );
    }
}

#[test]
fn partial_hints_use_managed_models_but_never_override_an_explicit_model_root() {
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    install(&fixture, &catalog);
    let mut hints = proposed_tuple(DEFAULT_OCR_PROFILE_ID, None).unwrap();
    let view = inspect_with(
        &catalog,
        &fixture.0,
        Some(&hints),
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(view.items[0].state, "verified");
    assert!(view.environment.is_none());
    assert_eq!(
        view.resolved.model_root.as_deref(),
        fixture.installed(&catalog).to_str()
    );
    assert!(view.resolved.runtime_path.is_none());
    assert!(view.resolved.native_library_paths.is_none());
    hints.model_root = fixture
        .0
        .join("explicit-missing-models")
        .to_str()
        .unwrap()
        .into();
    let view = inspect_with(
        &catalog,
        &fixture.0,
        Some(&hints),
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(view.items[0].state, "missing");
    assert!(view.environment.is_none());
    assert!(view.resolved.model_root.is_none());
    assert!(fixture.installed(&catalog).exists());
}

#[test]
fn relative_configuration_root_rechecks_managed_models_but_refuses_relative_manual_paths() {
    let fixture = Fixture::relative();
    let catalog = fixture_catalog();
    install(&fixture, &catalog);
    let cancel = AtomicBool::new(false);
    let view = inspect_with(&catalog, &fixture.0, None, None, &cancel).unwrap();
    assert_eq!(view.items[0].state, "verified");
    assert_eq!(
        view.resolved.model_root.as_deref(),
        fixture.installed(&catalog).canonicalize().unwrap().to_str()
    );
    assert!(view.environment.is_none());
    assert!(view.resolved.runtime_path.is_none());
    assert!(view.resolved.native_library_paths.is_none());

    let mut hints = proposed_tuple(DEFAULT_OCR_PROFILE_ID, None).unwrap();
    hints.model_root = fixture.installed(&catalog).to_str().unwrap().into();
    let view = inspect_with(&catalog, &fixture.0, Some(&hints), None, &cancel).unwrap();
    assert_eq!(view.items[0].state, "incompatible");
    assert!(view.resolved.model_root.is_none());
    assert!(view.environment.is_none());
}
