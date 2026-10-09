use super::*;
use catalog::Asset;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::sync::atomic::AtomicU64;
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);
const CONTENT: &[u8] = b"deterministic model transfer fixture";

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
    let mut catalog = Catalog::load().unwrap();
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
fn embedded_catalog_is_offline_valid_and_profiles_share_content() {
    let catalog = Catalog::load().unwrap();
    assert_eq!(catalog.profiles.len(), 2);
    assert_eq!(catalog.models.assets.len(), 2);
    let view = catalog_view().unwrap();
    assert!(view.environment.is_none());
    assert_eq!(
        view.items.iter().filter(|item| item.downloadable).count(),
        1
    );
    assert_eq!(view.items[0].state, "missing");
    assert!(
        guidance_link("rapidocr-models", 0)
            .unwrap()
            .starts_with("https://")
    );
    assert!(guidance_link("caller-url", 0).is_err());
    assert!(guidance_command("rapidocr-models", 0).is_err());
}

#[test]
fn catalog_transport_and_guidance_can_change_without_new_model_installations() {
    let original = Catalog::load().unwrap();
    let mut changed = original.clone();
    changed.models.assets[0].url = "https://example.com/reviewed-fixed-model.onnx".into();
    changed.platforms[0].native.commands = vec!["brew info opencv@4".into()];
    changed.models.name.en = "Updated model title".into();
    changed.validate().unwrap();
    assert_eq!(changed.installation_name(), original.installation_name());
    assert_eq!(changed.items()[0].name.en, "Updated model title");
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
    catalog = original.clone();
    catalog.profiles[0] = "arbitrary-profile".into();
    assert!(catalog.validate().is_err());
    catalog = original.clone();
    catalog.platforms[0].arch = "x86_64".into();
    assert!(catalog.validate().is_err());
    catalog = original.clone();
    catalog.platforms[0].native.required_modules.clear();
    assert!(catalog.validate().is_err());
    catalog = original;
    catalog.models.license.clear();
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
    let mut hints = proposed_tuple(None).unwrap();
    hints.profile = BOUNDED_PROFILE.into();
    hints.model = BOUNDED_PROFILE.into();
    hints.model_root = "/missing-models".into();
    assert_eq!(
        proposed_tuple(Some(&hints)).unwrap().profile,
        BOUNDED_PROFILE
    );
    hints.provider = "cuda".into();
    assert!(proposed_tuple(Some(&hints)).is_err());
    hints.provider.clear();
    hints.profile = "custom-model".into();
    assert!(proposed_tuple(Some(&hints)).is_err());
}

#[test]
fn absent_resources_do_not_create_directories_or_propose_environment() {
    let fixture = Fixture::new();
    let view = inspect(&fixture.0, None, &AtomicBool::new(false)).unwrap();
    assert!(view.environment.is_none());
    assert_eq!(view.items[0].state, "missing");
    assert!(!fixture.0.join("ocr-resources").exists());
    let error = download(
        &fixture.0,
        "https://caller.example/model",
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err();
    assert_eq!(error.context["stage"], "resource");
    assert!(!fixture.0.join("ocr-resources").exists());
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
    for stage in ["downloading", "publishing", "complete"] {
        let fixture = Fixture::new();
        let catalog = fixture_catalog();
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
        } else {
            assert_eq!(result.unwrap_err().category, "OcrSetupCancelled");
            assert!(!fixture.installed(&catalog).exists());
        }
        assert!(!fixture.staging().exists());
    }
}

#[test]
fn verified_installation_is_reused_without_fetch_and_corrupt_one_is_not_overwritten() {
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    install(&fixture, &catalog);
    download::verify_receipt(&fixture.installed(&catalog), &catalog).unwrap();
    download::acquire_with(
        &fixture.0,
        &catalog,
        &AtomicBool::new(false),
        |_| {},
        |_, _, _, _| panic!("must reuse verified files"),
    )
    .unwrap();
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
    let mut catalog = fixture_catalog();
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
    let mut hints = proposed_tuple(None).unwrap();
    hints.profile = BOUNDED_PROFILE.into();
    hints.model = BOUNDED_PROFILE.into();
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
    let result = inspect_with(&catalog, &fixture.0, Some(&hints), &AtomicBool::new(false)).unwrap();
    let proposal = result.environment.unwrap();
    assert_eq!(proposal.profile, BOUNDED_PROFILE);
    assert_eq!(
        proposal.model_root,
        fixture.installed(&catalog).to_str().unwrap()
    );
    assert!(hints.model_root.is_empty());
    fs::write(runtime, "wrong runtime").unwrap();
    let result = inspect_with(&catalog, &fixture.0, Some(&hints), &AtomicBool::new(false)).unwrap();
    assert!(result.environment.is_none());
    assert_eq!(result.items[0].state, "verified");
    assert_eq!(result.items[1].state, "incompatible");
    assert_eq!(result.items[2].state, "verified");
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
    fs::write(installed.join("receipt.json"), "{\"schema\":").unwrap();
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
    assert_eq!(
        fs::read(installed.join(&catalog.models.assets[0].path)).unwrap(),
        CONTENT
    );
}

#[test]
fn partial_hints_use_managed_models_but_never_override_an_explicit_model_root() {
    let fixture = Fixture::new();
    let catalog = fixture_catalog();
    install(&fixture, &catalog);
    let mut hints = proposed_tuple(None).unwrap();
    let view = inspect_with(&catalog, &fixture.0, Some(&hints), &AtomicBool::new(false)).unwrap();
    assert_eq!(view.items[0].state, "verified");
    assert!(view.environment.is_none());
    hints.model_root = fixture
        .0
        .join("explicit-missing-models")
        .to_str()
        .unwrap()
        .into();
    let view = inspect_with(&catalog, &fixture.0, Some(&hints), &AtomicBool::new(false)).unwrap();
    assert_eq!(view.items[0].state, "missing");
    assert!(view.environment.is_none());
    assert!(fixture.installed(&catalog).exists());
}
