use super::*;
use crate::ocr_setup::catalog::ArchiveFormat;
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::SimpleFileOptions;

static NEXT: AtomicU64 = AtomicU64::new(0);
const BODIES: [&[u8]; 3] = [
    b"runtime binary fixture",
    b"license fixture",
    b"third-party notices fixture",
];

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mado-runtime-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn stage(&self) -> PathBuf {
        self.0.join("ocr-resources").join(STAGING)
    }
    fn destination(&self, platform: &Platform) -> PathBuf {
        self.0
            .join("ocr-resources")
            .join(platform.runtime_installation_name())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct Entry {
    name: String,
    body: Vec<u8>,
    kind: u8,
    link: String,
}
impl Entry {
    fn file(name: impl Into<String>, body: &[u8]) -> Self {
        Self {
            name: name.into(),
            body: body.into(),
            kind: b'0',
            link: String::new(),
        }
    }
}

fn entries(platform: &Platform) -> Vec<Entry> {
    platform
        .runtime
        .archive
        .as_ref()
        .unwrap()
        .members
        .iter()
        .zip(BODIES)
        .map(|(member, body)| Entry::file(&member.archive_path, body))
        .collect()
}

fn tar_bytes(entries: &[Entry]) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    for entry in entries {
        let mut header = tar::Header::new_ustar();
        header.set_size(entry.body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::new(entry.kind));
        // Write raw header names so unsafe fixture paths reach our own admission boundary.
        let raw = header.as_mut_bytes();
        assert!(entry.name.len() <= 100 && entry.link.len() <= 100);
        raw[..entry.name.len()].copy_from_slice(entry.name.as_bytes());
        raw[157..157 + entry.link.len()].copy_from_slice(entry.link.as_bytes());
        header.set_cksum();
        archive.append(&header, Cursor::new(&entry.body)).unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

fn zip_bytes(entries: &[Entry]) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for entry in entries {
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o600);
        archive.start_file(&entry.name, options).unwrap();
        archive.write_all(&entry.body).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

fn fixture_catalog(os: &str) -> (Catalog, usize, Vec<u8>) {
    let mut catalog = Catalog::load().unwrap();
    let index = catalog
        .platforms
        .iter()
        .position(|platform| platform.os == os)
        .unwrap();
    let platform = &mut catalog.platforms[index];
    let spec = platform.runtime.archive.as_mut().unwrap();
    for (member, body) in spec.members.iter_mut().zip(BODIES) {
        member.bytes = body.len() as u64;
        member.sha256 = format!("{:x}", Sha256::digest(body));
    }
    platform.runtime.bytes = spec.members[0].bytes;
    platform.runtime.sha256.clone_from(&spec.members[0].sha256);
    let entries = entries(platform);
    let bytes = if os == "macos" {
        tar_bytes(&entries)
    } else {
        zip_bytes(&entries)
    };
    pin_archive(&mut catalog.platforms[index], &bytes);
    (catalog, index, bytes)
}

fn pin_archive(platform: &mut Platform, bytes: &[u8]) {
    let asset = &mut platform.runtime.archive.as_mut().unwrap().asset;
    asset.bytes = bytes.len() as u64;
    asset.sha256 = format!("{:x}", Sha256::digest(bytes));
}

fn install(
    fixture: &Fixture,
    catalog: &Catalog,
    index: usize,
    bytes: &[u8],
) -> Result<String, Fault> {
    acquire_with(
        &fixture.0,
        catalog,
        &catalog.platforms[index],
        &AtomicBool::new(false),
        |_| {},
        |asset, output, cancel, report| {
            download::transfer(&mut Cursor::new(bytes), output, asset, cancel, report)
        },
    )
}

#[test]
fn both_formats_publish_exact_members_and_reuse_without_fetching() {
    for os in ["macos", "windows"] {
        let fixture = Fixture::new();
        let (catalog, index, bytes) = fixture_catalog(os);
        let platform = &catalog.platforms[index];
        let path = install(&fixture, &catalog, index, &bytes).unwrap();
        assert_eq!(
            Path::new(&path),
            fixture
                .destination(platform)
                .join(&platform.runtime.filename)
        );
        assert!(!fixture.stage().exists());
        assert_eq!(
            fs::read_dir(fixture.destination(platform)).unwrap().count(),
            4
        );
        for (member, body) in specification(platform).unwrap().members.iter().zip(BODIES) {
            assert_eq!(
                fs::read(fixture.destination(platform).join(&member.path)).unwrap(),
                body
            );
        }
        let reused = acquire_with(
            &fixture.0,
            &catalog,
            platform,
            &AtomicBool::new(false),
            |_| {},
            |_, _, _, _| panic!("verified runtime must not fetch again"),
        )
        .unwrap();
        assert_eq!(reused, path);
        let lease = download::recover(&fixture.0, &catalog).unwrap();
        if platform.os == std::env::consts::OS && platform.arch == std::env::consts::ARCH {
            // Parent inspection already owns the lease: installed must not recursively lock.
            assert_eq!(
                installed(&fixture.0, &catalog, &AtomicBool::new(false)).unwrap(),
                path
            );
        }
        drop(lease);
    }
}

#[test]
fn selected_files_and_receipt_are_all_required_for_reuse_without_repair() {
    for name in [
        "runtime",
        "LICENSE",
        "ThirdPartyNotices.txt",
        "receipt.json",
    ] {
        let fixture = Fixture::new();
        let (catalog, index, bytes) = fixture_catalog("macos");
        let platform = &catalog.platforms[index];
        install(&fixture, &catalog, index, &bytes).unwrap();
        let name = if name == "runtime" {
            &platform.runtime.filename
        } else {
            name
        };
        let path = fixture.destination(platform).join(name);
        fs::write(&path, b"changed publication").unwrap();
        assert!(
            acquire_with(
                &fixture.0,
                &catalog,
                platform,
                &AtomicBool::new(false),
                |_| {},
                |_, _, _, _| panic!("must not repair immutable publication")
            )
            .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), b"changed publication");
    }
}

#[test]
fn archive_and_selected_member_integrity_failures_never_publish() {
    for os in ["macos", "windows"] {
        for failure in [
            "archive_hash",
            "archive_size",
            "member_hash",
            "member_size",
            "missing_notice",
        ] {
            let fixture = Fixture::new();
            let (mut catalog, index, mut bytes) = fixture_catalog(os);
            match failure {
                "archive_hash" => {
                    catalog.platforms[index]
                        .runtime
                        .archive
                        .as_mut()
                        .unwrap()
                        .asset
                        .sha256 = "0".repeat(64)
                }
                "archive_size" => {
                    catalog.platforms[index]
                        .runtime
                        .archive
                        .as_mut()
                        .unwrap()
                        .asset
                        .bytes += 1
                }
                "member_hash" => {
                    catalog.platforms[index]
                        .runtime
                        .archive
                        .as_mut()
                        .unwrap()
                        .members[2]
                        .sha256 = "0".repeat(64)
                }
                "member_size" => {
                    catalog.platforms[index]
                        .runtime
                        .archive
                        .as_mut()
                        .unwrap()
                        .members[2]
                        .bytes += 1
                }
                "missing_notice" => {
                    let mut members = entries(&catalog.platforms[index]);
                    members.pop();
                    bytes = if os == "macos" {
                        tar_bytes(&members)
                    } else {
                        zip_bytes(&members)
                    };
                    pin_archive(&mut catalog.platforms[index], &bytes);
                }
                _ => unreachable!(),
            }
            assert!(
                install(&fixture, &catalog, index, &bytes).is_err(),
                "{os}: {failure}"
            );
            assert!(!fixture.destination(&catalog.platforms[index]).exists());
            assert!(!fixture.stage().exists());
        }
    }
}

#[test]
fn verified_staged_bytes_are_required_even_if_transport_claims_success() {
    let fixture = Fixture::new();
    let (catalog, index, _) = fixture_catalog("macos");
    let error = acquire_with(
        &fixture.0,
        &catalog,
        &catalog.platforms[index],
        &AtomicBool::new(false),
        |_| {},
        |_, file, _, _| {
            file.write_all(b"not the accepted archive").unwrap();
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.context["stage"], "integrity");
    assert!(!fixture.stage().exists());
}

#[test]
fn tar_ignores_only_reviewed_unselected_alias_and_normalizes_official_dot_prefix() {
    let fixture = Fixture::new();
    let (mut catalog, index, _) = fixture_catalog("macos");
    let mut members = entries(&catalog.platforms[index]);
    members.push(Entry {
        name: "onnxruntime-osx-arm64-1.29.0/lib/libonnxruntime.1.dylib".into(),
        body: Vec::new(),
        kind: b'2',
        link: "libonnxruntime.1.29.0.dylib".into(),
    });
    members.push(Entry::file(
        "onnxruntime-osx-arm64-1.29.0/unselected.txt",
        b"not installed",
    ));
    for member in &mut members {
        member.name.insert_str(0, "./");
    }
    let bytes = tar_bytes(&members);
    pin_archive(&mut catalog.platforms[index], &bytes);
    install(&fixture, &catalog, index, &bytes).unwrap();
    let destination = fixture.destination(&catalog.platforms[index]);
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 4);
    assert!(!destination.join("libonnxruntime.1.dylib").exists());
    assert!(!destination.join("unselected.txt").exists());
}

#[test]
fn tar_refuses_unsafe_duplicates_selected_links_and_special_entries() {
    for mutation in [
        "traversal",
        "absolute",
        "backslash",
        "duplicate",
        "case_alias",
        "selected_symlink",
        "hardlink",
        "escaping_alias",
        "fifo",
        "oversized",
        "too_many",
    ] {
        let fixture = Fixture::new();
        let (mut catalog, index, _) = fixture_catalog("macos");
        let mut members = entries(&catalog.platforms[index]);
        match mutation {
            "traversal" => members[0].name = "../escape".into(),
            "absolute" => members[0].name = "/escape".into(),
            "backslash" => members[0].name = "onnxruntime-osx-arm64-1.29.0\\escape".into(),
            "duplicate" => members.push(members[0].clone()),
            "case_alias" => {
                let mut alias = members[0].clone();
                alias.name = alias.name.to_uppercase();
                members.push(alias);
            }
            "selected_symlink" | "hardlink" => {
                members[0].kind = if mutation == "hardlink" { b'1' } else { b'2' };
                members[0].link = "LICENSE".into();
                members[0].body.clear();
            }
            "escaping_alias" => members.push(Entry {
                name: "onnxruntime-osx-arm64-1.29.0/lib/libonnxruntime.1.dylib".into(),
                body: Vec::new(),
                kind: b'2',
                link: "../../outside".into(),
            }),
            "fifo" => members.push(Entry {
                name: "onnxruntime-osx-arm64-1.29.0/fifo".into(),
                body: Vec::new(),
                kind: b'6',
                link: String::new(),
            }),
            "oversized" => members[0].body.push(0),
            "too_many" => {
                for i in 0..257 {
                    members.push(Entry::file(
                        format!("onnxruntime-osx-arm64-1.29.0/entry-{i}"),
                        b"",
                    ));
                }
            }
            _ => unreachable!(),
        }
        let bytes = tar_bytes(&members);
        pin_archive(&mut catalog.platforms[index], &bytes);
        assert!(
            install(&fixture, &catalog, index, &bytes).is_err(),
            "{mutation}"
        );
        assert!(!fixture.destination(&catalog.platforms[index]).exists());
        assert!(!fixture.stage().exists());
        assert!(!fixture.0.join("escape").exists());
    }
}

fn mutate_zip_directory(bytes: &mut [u8], operation: impl FnOnce(&mut [u8])) {
    let footer = bytes.len() - 22;
    let offset = u32::from_le_bytes(bytes[footer + 16..footer + 20].try_into().unwrap()) as usize;
    operation(&mut bytes[offset..]);
}

#[test]
fn zip_refuses_duplicate_unsafe_link_and_expansion_destinations_before_publication() {
    for mutation in [
        "duplicate",
        "traversal",
        "absolute",
        "drive",
        "backslash",
        "reserved",
        "symlink",
        "oversized",
        "too_many",
    ] {
        let fixture = Fixture::new();
        let (mut catalog, index, _) = fixture_catalog("windows");
        let mut members = entries(&catalog.platforms[index]);
        match mutation {
            "duplicate" => {
                members.push(Entry::file("onnxruntime-win-x64-1.29.0/LICENSe", b"alias"))
            }
            "traversal" => members[0].name = "../escape".into(),
            "absolute" => members[0].name = "/escape".into(),
            "drive" => members[0].name = "C:/escape".into(),
            "backslash" => members[0].name = "onnxruntime-win-x64-1.29.0\\escape".into(),
            "reserved" => members[0].name = "onnxruntime-win-x64-1.29.0/CON.dll".into(),
            "too_many" => {
                for i in 0..257 {
                    members.push(Entry::file(
                        format!("onnxruntime-win-x64-1.29.0/entry-{i}"),
                        b"",
                    ));
                }
            }
            _ => {}
        }
        let mut bytes = zip_bytes(&members);
        if mutation == "duplicate" {
            // Exact duplicate names, including central records, bypass ZipWriter's own refusal.
            let needle = b"LICENSe";
            for i in 0..bytes.len() - needle.len() {
                if &bytes[i..i + needle.len()] == needle {
                    bytes[i + needle.len() - 1] = b'E';
                }
            }
        } else if mutation == "symlink" {
            mutate_zip_directory(&mut bytes, |header| {
                header[38..42].copy_from_slice(&(0o120777_u32 << 16).to_le_bytes())
            });
        } else if mutation == "oversized" {
            mutate_zip_directory(&mut bytes, |header| {
                header[24..28].copy_from_slice(&(513_u32 * 1024 * 1024).to_le_bytes())
            });
        }
        pin_archive(&mut catalog.platforms[index], &bytes);
        assert!(
            install(&fixture, &catalog, index, &bytes).is_err(),
            "{mutation}"
        );
        assert!(!fixture.destination(&catalog.platforms[index]).exists());
        assert!(!fixture.stage().exists());
        assert!(!fixture.0.join("escape").exists());
    }
}

#[test]
fn tar_end_padding_cannot_bypass_the_total_expansion_bound() {
    let fixture = Fixture::new();
    let (mut catalog, index, bytes) = fixture_catalog("macos");
    let mut original = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    std::io::copy(&mut original, &mut encoder).unwrap();
    std::io::copy(
        &mut std::io::repeat(0).take(512 * 1024 * 1024),
        &mut encoder,
    )
    .unwrap();
    let bytes = encoder.finish().unwrap();
    pin_archive(&mut catalog.platforms[index], &bytes);
    let error = install(&fixture, &catalog, index, &bytes).unwrap_err();
    assert!(error.message.contains("expansion bound"), "{error:?}");
    assert!(!fixture.destination(&catalog.platforms[index]).exists());
    assert!(!fixture.stage().exists());
}

#[test]
fn cancellation_after_extraction_creates_a_member_preserves_typed_stop_and_cleanup() {
    struct StopAfterMember<'a> {
        input: Cursor<Vec<u8>>,
        member: PathBuf,
        cancel: &'a AtomicBool,
    }
    impl Read for StopAfterMember<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.member.exists() {
                self.cancel.store(true, Ordering::Release);
                return Err(std::io::Error::other("stop during extraction"));
            }
            // Prevent gzip read-ahead from consuming the archive before member creation.
            let length = buffer.len().min(1);
            self.input.read(&mut buffer[..length])
        }
    }
    impl Seek for StopAfterMember<'_> {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.input.seek(position)
        }
    }
    let fixture = Fixture::new();
    let (catalog, index, bytes) = fixture_catalog("macos");
    let platform = &catalog.platforms[index];
    let spec = platform.runtime.archive.as_ref().unwrap();
    let owned = download::owned(&fixture.0, true).unwrap().unwrap();
    fs::create_dir(fixture.stage()).unwrap();
    let member = fixture.stage().join(&spec.members[0].path);
    let cancel = AtomicBool::new(false);
    let error = archive::extract(
        StopAfterMember {
            input: Cursor::new(bytes),
            member: member.clone(),
            cancel: &cancel,
        },
        &fixture.stage(),
        spec,
        &cancel,
    )
    .unwrap_err();
    assert!(member.exists());
    assert_eq!(error.category, "OcrSetupCancelled");
    assert!(!fixture.destination(platform).exists());
    cleanup(&owned, &catalog).unwrap();
    assert!(!fixture.stage().exists());
}

#[test]
fn cancellation_before_commit_cleans_stage_and_late_cancellation_keeps_publication() {
    for stage in [
        "resolving",
        "downloading",
        "verifying",
        "publishing",
        "complete",
    ] {
        let fixture = Fixture::new();
        let (catalog, index, bytes) = fixture_catalog("macos");
        let cancel = AtomicBool::new(false);
        let result = acquire_with(
            &fixture.0,
            &catalog,
            &catalog.platforms[index],
            &cancel,
            |progress| {
                if progress.stage == stage {
                    cancel.store(true, Ordering::Release);
                }
            },
            |asset, output, cancel, report| {
                download::transfer(&mut Cursor::new(&bytes), output, asset, cancel, report)
            },
        );
        if stage == "complete" {
            assert!(result.is_ok());
            assert!(fixture.destination(&catalog.platforms[index]).exists());
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.category, "OcrSetupCancelled");
            assert!(error.context.get("cleanup_error").is_none());
            assert!(!fixture.destination(&catalog.platforms[index]).exists());
        }
        assert!(!fixture.stage().exists());
    }
}

#[test]
fn publication_conflict_is_not_replaced_and_stage_is_removed() {
    let fixture = Fixture::new();
    let (catalog, index, bytes) = fixture_catalog("macos");
    let destination = fixture.destination(&catalog.platforms[index]);
    let error = acquire_with(
        &fixture.0,
        &catalog,
        &catalog.platforms[index],
        &AtomicBool::new(false),
        |progress| {
            if progress.stage == "publishing" {
                fs::write(&destination, b"preserve conflict").unwrap();
            }
        },
        |asset, output, cancel, report| {
            download::transfer(&mut Cursor::new(&bytes), output, asset, cancel, report)
        },
    )
    .unwrap_err();
    assert_eq!(error.context["stage"], "publication");
    assert_eq!(fs::read(destination).unwrap(), b"preserve conflict");
    assert!(!fixture.stage().exists());
}

#[test]
fn model_and_runtime_publications_coexist_and_recovery_reclaims_both_known_stages() {
    let fixture = Fixture::new();
    let (mut catalog, index, bytes) = fixture_catalog("macos");
    let model = b"local model fixture";
    for asset in &mut catalog.models.assets {
        asset.bytes = model.len() as u64;
        asset.sha256 = format!("{:x}", Sha256::digest(model));
    }
    let runtime = install(&fixture, &catalog, index, &bytes).unwrap();
    download::acquire_with(
        &fixture.0,
        &catalog,
        &AtomicBool::new(false),
        |_| {},
        |asset, output, cancel, report| {
            download::transfer(&mut Cursor::new(model), output, asset, cancel, report)
        },
    )
    .unwrap();
    let models = fixture
        .0
        .join("ocr-resources")
        .join(catalog.installation_name());
    let model_stage = fixture.0.join("ocr-resources/staging-v1");
    fs::create_dir(&model_stage).unwrap();
    fs::write(model_stage.join("receipt.json"), b"interrupted model").unwrap();
    fs::create_dir(fixture.stage()).unwrap();
    fs::write(fixture.stage().join(ARCHIVE), b"interrupted archive").unwrap();
    let lease = download::recover(&fixture.0, &catalog).unwrap();
    assert!(!fixture.stage().exists() && !model_stage.exists());
    files::models(&models, &catalog, &AtomicBool::new(false)).unwrap();
    download::verify_receipt(&models, &catalog).unwrap();
    assert_eq!(
        verify_installation(
            &fixture.destination(&catalog.platforms[index]),
            &catalog,
            &catalog.platforms[index],
            &AtomicBool::new(false)
        )
        .unwrap(),
        runtime
    );
    drop(lease);
    assert_eq!(install(&fixture, &catalog, index, &bytes).unwrap(), runtime);
}

#[test]
fn active_lease_refuses_runtime_acquisition_without_reclaiming_staging() {
    let fixture = Fixture::new();
    let (catalog, index, bytes) = fixture_catalog("macos");
    let owned = download::owned(&fixture.0, true).unwrap().unwrap();
    let lease = download::lease(&owned).unwrap();
    fs::create_dir(fixture.stage()).unwrap();
    fs::write(fixture.stage().join(ARCHIVE), b"active download").unwrap();
    assert_eq!(
        install(&fixture, &catalog, index, &bytes)
            .unwrap_err()
            .category,
        "OcrSetupBusy"
    );
    assert_eq!(
        fs::read(fixture.stage().join(ARCHIVE)).unwrap(),
        b"active download"
    );
    drop(lease);
}

#[test]
fn cancelled_transfer_with_unknown_stage_entry_reports_cleanup_fault_and_preserves_it() {
    let fixture = Fixture::new();
    let (catalog, index, _) = fixture_catalog("macos");
    let cancel = AtomicBool::new(false);
    let error = acquire_with(
        &fixture.0,
        &catalog,
        &catalog.platforms[index],
        &cancel,
        |_| {},
        |_, _, cancel, _| {
            fs::write(fixture.stage().join("user-data"), b"preserve").unwrap();
            cancel.store(true, Ordering::Release);
            check_cancel(cancel)
        },
    )
    .unwrap_err();
    assert_eq!(error.category, "OcrSetupCancelled");
    assert!(error.context["cleanup_error"].is_object());
    assert_eq!(
        fs::read(fixture.stage().join("user-data")).unwrap(),
        b"preserve"
    );
    assert!(
        download::recover(&fixture.0, &catalog).unwrap_err().context["cleanup_error"].is_object()
    );
}

#[cfg(unix)]
#[test]
fn installed_or_staged_links_are_refused_without_following_or_deleting_targets() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let (catalog, index, bytes) = fixture_catalog("macos");
    install(&fixture, &catalog, index, &bytes).unwrap();
    let external = fixture.0.join("external-notice");
    fs::write(&external, BODIES[2]).unwrap();
    let notice = fixture
        .destination(&catalog.platforms[index])
        .join("ThirdPartyNotices.txt");
    fs::remove_file(&notice).unwrap();
    symlink(&external, &notice).unwrap();
    assert!(install(&fixture, &catalog, index, &bytes).is_err());
    fs::create_dir(fixture.stage()).unwrap();
    symlink(&external, fixture.stage().join(ARCHIVE)).unwrap();
    assert!(download::recover(&fixture.0, &catalog).is_err());
    assert_eq!(fs::read(&external).unwrap(), BODIES[2]);
    assert!(
        fs::symlink_metadata(fixture.stage().join(ARCHIVE))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn catalog_refuses_unreviewed_archives_members_and_native_search_scopes() {
    for mutation in [
        "archive_hash",
        "runtime_hash",
        "member",
        "duplicate",
        "root",
        "relative_root",
        "subdirectory",
        "too_many",
    ] {
        let mut catalog = Catalog::load().unwrap();
        let platform = &mut catalog.platforms[0];
        match mutation {
            "archive_hash" => {
                platform.runtime.archive.as_mut().unwrap().asset.sha256 = "0".repeat(64)
            }
            "runtime_hash" => platform.runtime.sha256 = "0".repeat(64),
            "member" => {
                platform.runtime.archive.as_mut().unwrap().members[1].archive_path =
                    "../LICENSE".into()
            }
            "duplicate" => {
                let spec = platform.runtime.archive.as_mut().unwrap();
                spec.members[2] = spec.members[1].clone();
            }
            "root" => platform.native.search_roots.push("/".into()),
            "relative_root" => platform.native.search_roots.push("usr/local".into()),
            "subdirectory" => platform
                .native
                .search_subdirectories
                .push("../outside".into()),
            "too_many" => {
                platform.native.search_subdirectories = (0..9).map(|i| format!("dir-{i}")).collect()
            }
            _ => unreachable!(),
        }
        assert!(catalog.validate().is_err(), "{mutation}");
    }
    assert_eq!(
        Catalog::load().unwrap().platforms[0]
            .runtime
            .archive
            .as_ref()
            .unwrap()
            .format,
        ArchiveFormat::Tgz
    );
}

#[test]
fn official_directory_layout_is_inspected_but_only_flat_selected_files_are_installed() {
    for os in ["macos", "windows"] {
        let fixture = Fixture::new();
        let (mut catalog, index, _) = fixture_catalog(os);
        let platform = &catalog.platforms[index];
        let members = entries(platform);
        let prefix = specification(platform).unwrap().members[0]
            .archive_path
            .split('/')
            .next()
            .unwrap();
        let bytes = if os == "macos" {
            let mut all = vec![
                Entry {
                    name: format!("./{prefix}/"),
                    body: Vec::new(),
                    kind: b'5',
                    link: String::new(),
                },
                Entry {
                    name: format!("./{prefix}/lib/"),
                    body: Vec::new(),
                    kind: b'5',
                    link: String::new(),
                },
            ];
            all.extend(members);
            tar_bytes(&all)
        } else {
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            writer.add_directory(format!("{prefix}/"), options).unwrap();
            writer
                .add_directory(format!("{prefix}/lib/"), options)
                .unwrap();
            for member in members {
                writer
                    .start_file(
                        &member.name,
                        SimpleFileOptions::default()
                            .compression_method(zip::CompressionMethod::Deflated),
                    )
                    .unwrap();
                writer.write_all(&member.body).unwrap();
            }
            writer.finish().unwrap().into_inner()
        };
        pin_archive(&mut catalog.platforms[index], &bytes);
        install(&fixture, &catalog, index, &bytes).unwrap();
        assert_eq!(
            fs::read_dir(fixture.destination(&catalog.platforms[index]))
                .unwrap()
                .count(),
            4
        );
    }
}

#[test]
fn truncated_archives_and_file_directory_collisions_are_not_published() {
    for os in ["macos", "windows"] {
        for failure in ["truncated", "collision"] {
            let fixture = Fixture::new();
            let (mut catalog, index, mut bytes) = fixture_catalog(os);
            if failure == "truncated" {
                bytes.truncate(bytes.len() - 8);
            } else {
                let mut members = entries(&catalog.platforms[index]);
                let child = format!("{}/child", members[0].name);
                members.push(Entry::file(child, b"cannot be a child of a file"));
                bytes = if os == "macos" {
                    tar_bytes(&members)
                } else {
                    zip_bytes(&members)
                };
            }
            pin_archive(&mut catalog.platforms[index], &bytes);
            assert!(
                install(&fixture, &catalog, index, &bytes).is_err(),
                "{os}: {failure}"
            );
            assert!(!fixture.destination(&catalog.platforms[index]).exists());
            assert!(!fixture.stage().exists());
        }
    }
}
