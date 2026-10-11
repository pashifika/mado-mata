//! Key custody regressions on real files: publication, reopen, refusal, cancellation,
//! filesystem failures, offline signing and the configuration capture boundary.
use super::*;
use crate::backup;
use crate::configuration::tests::Root;
use crate::configuration::{Capture, capture};
use crate::restore;
use crate::storage::Settings;
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::Value;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

fn account() -> Account {
    Account {
        issuer: oidc::GOOGLE.issuer.to_owned(),
        subject: "subject-under-test".to_owned(),
        client_id: oidc::GOOGLE.client_id.to_owned(),
        display_label: "Author".to_owned(),
    }
}

fn publish_now(root: &Path) -> Result<SigningInfo, Fault> {
    publish(root, &account(), &AtomicBool::new(false), |_| Ok(()))
}

fn active(root: &Path) -> PathBuf {
    root.join(KEYS).join(ACTIVE)
}

fn names(directory: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn record(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(active(root)).unwrap()).unwrap()
}

fn settings(limit: usize) -> Vec<u8> {
    serde_json::to_vec(&Settings {
        gui_log_limit: limit,
        ..Settings::default()
    })
    .unwrap()
}

#[test]
fn published_key_reopens_consistently_and_exposes_only_public_facts() {
    let root = Root::new();
    assert_eq!(status(&root.0).unwrap(), None);
    assert_eq!(
        Signer::open(&root.0).unwrap_err().category,
        "SigningKeyMissing"
    );
    fs::create_dir(root.0.join(KEYS)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.0.join(KEYS), fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(status(&root.0).unwrap(), None);

    let info = publish_now(&root.0).unwrap();
    let signer = Signer::open(&root.0).unwrap();
    assert_eq!(info, status(&root.0).unwrap().unwrap());
    assert_eq!(info.key_id, signer.key_id());
    assert_eq!(signer.key_id(), digest(&signer.public_key()));
    assert_eq!(signer.key_id().len(), 64);
    assert!(
        signer
            .key_id()
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    );
    assert_eq!(
        (info.provider, info.local_only, info.display_label.as_str()),
        ("google", true, "Author")
    );
    let public = serde_json::to_value(&info).unwrap();
    assert_eq!(
        public
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(
            [
                "display_label",
                "enrolled_at",
                "key_id",
                "local_only",
                "provider"
            ]
            .map(String::from)
        ),
    );
    let text = public.to_string();
    assert!(!text.contains("subject-under-test") && !text.contains(oidc::GOOGLE.client_id));
    assert!(!format!("{signer:?}").contains(&record(&root.0)["seed"].as_str().unwrap()[..8]));

    let stored = record(&root.0);
    assert_eq!(
        stored
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(
            [
                "algorithm",
                "client_id",
                "display_label",
                "enrolled_at",
                "issuer",
                "key_id",
                "public_key",
                "seed",
                "subject",
                "version",
            ]
            .map(String::from)
        ),
    );
    assert_eq!(
        STANDARD
            .decode(stored["public_key"].as_str().unwrap())
            .unwrap(),
        signer.public_key()
    );
    let bytes = fs::read(active(&root.0)).unwrap();
    assert!(bytes.len() <= MAX_RECORD_BYTES);
    let reopened = Signer::open(&root.0).unwrap();
    assert_eq!(reopened.public_key(), signer.public_key());
    assert_eq!(fs::read(active(&root.0)).unwrap(), bytes);
    assert_eq!(names(&root.0.join(KEYS)), [ACTIVE]);
}

#[cfg(unix)]
#[test]
fn key_store_modes_are_set_and_checked_without_changing_the_root() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
    let root = Root::new();
    let keys = root.0.join(KEYS);
    fs::create_dir(&keys).unwrap();
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o755)).unwrap();
    let root_mode = mode(&root.0);
    publish_now(&root.0).unwrap();
    assert_eq!(mode(&keys), 0o700);
    assert_eq!(mode(&active(&root.0)), 0o600);
    assert_eq!(mode(&root.0), root_mode);

    // Later loosening is refused, not silently repaired by reading.
    fs::set_permissions(active(&root.0), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(status(&root.0).is_err());
    assert!(Signer::open(&root.0).is_err());
    assert_eq!(mode(&active(&root.0)), 0o644);
    fs::set_permissions(active(&root.0), fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o750)).unwrap();
    assert!(status(&root.0).is_err());
    assert_eq!(mode(&keys), 0o750);
}

#[cfg(unix)]
#[test]
fn linked_key_records_and_stores_are_refused() {
    use std::os::unix::fs::symlink;
    let root = Root::new();
    publish_now(&root.0).unwrap();
    let bytes = fs::read(active(&root.0)).unwrap();
    let outside = Root::new();
    outside.put("active.json", &bytes);
    fs::remove_file(active(&root.0)).unwrap();
    symlink(outside.0.join("active.json"), active(&root.0)).unwrap();
    assert!(status(&root.0).is_err());
    fs::remove_file(active(&root.0)).unwrap();
    fs::hard_link(outside.0.join("active.json"), active(&root.0)).unwrap();
    assert!(status(&root.0).is_err());
    assert!(Signer::open(&root.0).is_err());

    let linked = Root::new();
    symlink(&outside.0, linked.0.join(KEYS)).unwrap();
    assert!(status(&linked.0).is_err());
    assert_eq!(
        publish_now(&linked.0).unwrap_err().category,
        "SigningStorage"
    );
    assert_eq!(fs::read(outside.0.join("active.json")).unwrap(), bytes);
}

#[test]
fn unusable_records_fail_without_regeneration_or_login() {
    let root = Root::new();
    publish_now(&root.0).unwrap();
    let original = fs::read(active(&root.0)).unwrap();
    let valid = record(&root.0);
    let seed = valid["seed"].as_str().unwrap().to_owned();
    let other = Signer::from_seed([7; 32]);
    let mut cases = vec![
        original[..original.len() / 2].to_vec(),
        Vec::new(),
        b"not json".to_vec(),
        serde_json::to_vec(&json!([
            valid["version"],
            valid["algorithm"],
            valid["seed"],
            valid["public_key"],
            valid["key_id"],
            valid["issuer"],
            valid["subject"],
            valid["client_id"],
            valid["display_label"],
            valid["enrolled_at"]
        ]))
        .unwrap(),
        String::from_utf8(original.clone())
            .unwrap()
            .replacen('{', "{\"version\":1,", 1)
            .into_bytes(),
    ];
    for (field, value) in [
        ("version", json!(2)),
        ("algorithm", json!("Ed448")),
        ("seed", json!(STANDARD.encode([7u8; 32]))),
        ("seed", json!("AAAA")),
        ("seed", json!(format!("{}B=", "A".repeat(42)))),
        ("public_key", json!(STANDARD.encode(other.public_key()))),
        ("key_id", json!(other.key_id())),
        (
            "key_id",
            json!(valid["key_id"].as_str().unwrap().to_uppercase()),
        ),
        ("issuer", json!("https://issuer.example")),
        ("subject", json!("")),
        ("client_id", json!("client\u{7}")),
        ("display_label", json!("x".repeat(MAX_LABEL_BYTES + 1))),
        ("display_label", json!("x".repeat(MAX_RECORD_BYTES))),
        ("enrolled_at", json!(-1)),
        ("enrolled_at", json!(u64::MAX)),
        ("extra", json!(true)),
    ] {
        let mut changed = valid.clone();
        changed[field] = value;
        cases.push(serde_json::to_vec(&changed).unwrap());
    }
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("key_id");
    cases.push(serde_json::to_vec(&missing).unwrap());

    for bytes in cases {
        fs::write(active(&root.0), &bytes).unwrap();
        let fault = status(&root.0).unwrap_err();
        assert!(
            matches!(fault.category.as_str(), "SigningKey" | "StorageLimit"),
            "{fault:?}"
        );
        assert!(!serde_json::to_string(&fault).unwrap().contains(&seed));
        assert!(Signer::open(&root.0).is_err());
        let opened = Cell::new(false);
        let refused = enroll(&root.0, &AtomicBool::new(false), |_| {
            opened.set(true);
            Ok(())
        })
        .unwrap_err();
        assert_eq!(refused.category, fault.category);
        assert!(!opened.get());
        assert_eq!(fs::read(active(&root.0)).unwrap(), bytes);
        assert_eq!(names(&root.0.join(KEYS)), [ACTIVE]);
    }
}

#[test]
fn publication_never_replaces_and_competing_attempts_install_one_key() {
    let root = Root::new();
    publish_now(&root.0).unwrap();
    let before = fs::read(active(&root.0)).unwrap();
    assert_eq!(
        publish_now(&root.0).unwrap_err().category,
        "SigningKeyExists"
    );
    let refused = enroll(&root.0, &AtomicBool::new(false), |_| {
        panic!("an existing key must refuse before login")
    })
    .unwrap_err();
    assert_eq!(refused.category, "SigningKeyExists");
    assert_eq!(fs::read(active(&root.0)).unwrap(), before);

    let shared = Arc::new(Root::new());
    let barrier = Arc::new(Barrier::new(8));
    let attempts: Vec<_> = (0..8)
        .map(|_| {
            let (root, barrier) = (shared.clone(), barrier.clone());
            thread::spawn(move || {
                barrier.wait();
                publish_now(&root.0)
            })
        })
        .collect();
    let results: Vec<_> = attempts
        .into_iter()
        .map(|attempt| attempt.join().unwrap())
        .collect();
    let winners: Vec<_> = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect();
    assert_eq!(winners.len(), 1);
    for refused in results.iter().filter_map(|result| result.as_ref().err()) {
        assert_eq!(refused.category, "SigningKeyExists");
    }
    assert_eq!(status(&shared.0).unwrap().as_ref(), Some(winners[0]));
    assert_eq!(names(&shared.0.join(KEYS)), [ACTIVE]);
}

#[test]
fn cancellation_at_the_publication_boundary_leaves_no_key_or_staging() {
    let root = Root::new();
    let cancelled = AtomicBool::new(false);
    let fault = publish(&root.0, &account(), &cancelled, |staging| {
        assert!(staging.is_file());
        cancelled.store(true, Ordering::Release);
        Ok(())
    })
    .unwrap_err();
    assert_eq!(fault.category, "SigningCancelled");
    assert!(fault.context.get("cleanup_error").is_none());
    assert!(fault.context.get("published").is_none());
    assert_eq!(status(&root.0).unwrap(), None);
    assert!(names(&root.0.join(KEYS)).is_empty());

    let fault = enroll(&root.0, &AtomicBool::new(true), |_| {
        panic!("a cancelled enrollment must not open the browser")
    })
    .unwrap_err();
    assert_eq!(fault.category, "SigningCancelled");
    assert_eq!(status(&root.0).unwrap(), None);
}

#[test]
fn filesystem_failures_are_reported_and_never_publish() {
    let occupied = Root::new();
    occupied.put(KEYS, b"user file");
    assert_eq!(
        publish_now(&occupied.0).unwrap_err().category,
        "SigningStorage"
    );
    assert_eq!(fs::read(occupied.0.join(KEYS)).unwrap(), b"user file");
    assert!(status(&occupied.0).is_err());

    let vanished = Root::new();
    let fault = publish(
        &vanished.0,
        &account(),
        &AtomicBool::new(false),
        |staging| {
            fs::remove_file(staging).unwrap();
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(fault.category, "SigningStorage");
    assert_eq!(fault.context["kind"], "NotFound");
    assert!(fault.context.get("cleanup_error").is_none());
    assert_eq!(status(&vanished.0).unwrap(), None);

    let raced = Root::new();
    let fault = publish(&raced.0, &account(), &AtomicBool::new(false), |_| {
        fs::write(active(&raced.0), b"competing writer").unwrap();
        Ok(())
    })
    .unwrap_err();
    assert_eq!(fault.category, "SigningKeyExists");
    assert_eq!(fs::read(active(&raced.0)).unwrap(), b"competing writer");
    assert_eq!(names(&raced.0.join(KEYS)), [ACTIVE]);

    let stuck = Root::new();
    let fault = publish(&stuck.0, &account(), &AtomicBool::new(false), |staging| {
        fs::remove_file(staging).unwrap();
        fs::create_dir(staging).unwrap();
        fs::write(staging.join("held"), b"held").unwrap();
        Err(Fault::new("Injected", "injected failure after staging"))
    })
    .unwrap_err();
    assert_eq!(fault.category, "Injected");
    let cleanup: Fault = serde_json::from_value(fault.context["cleanup_error"].clone()).unwrap();
    assert_eq!(cleanup.category, "SigningStorage");
    assert_eq!(status(&stuck.0).unwrap(), None);
}

#[test]
fn each_data_root_has_an_independent_identity_and_loss_needs_new_enrollment() {
    let (first, second) = (Root::new(), Root::new());
    let a = publish_now(&first.0).unwrap();
    let b = publish_now(&second.0).unwrap();
    assert_ne!(a.key_id, b.key_id);
    fs::remove_file(active(&first.0)).unwrap();
    assert_eq!(
        Signer::open(&first.0).unwrap_err().category,
        "SigningKeyMissing"
    );
    assert_eq!(status(&first.0).unwrap(), None);
    let c = publish_now(&first.0).unwrap();
    assert_ne!(c.key_id, a.key_id);
    assert_eq!(status(&second.0).unwrap(), Some(b));
}

#[test]
fn signing_is_offline_domain_bound_and_bounded() {
    let root = Root::new();
    let signer = test_signer(&root.0);
    let verifying = VerifyingKey::from_bytes(&signer.public_key()).unwrap();
    let index = br#"{"format":1,"files":[]}"#;
    let signature = Signature::from_bytes(&signer.sign_index(index).unwrap());
    let mut message = SIGNING_DOMAIN.to_vec();
    message.extend_from_slice(index);
    verifying.verify_strict(&message, &signature).unwrap();
    assert!(verifying.verify_strict(index, &signature).is_err());
    let mut changed = message.clone();
    *changed.last_mut().unwrap() ^= 1;
    assert!(verifying.verify_strict(&changed, &signature).is_err());
    assert_eq!(SIGNING_DOMAIN, b"madomata-pkg-v1\n");

    assert!(signer.sign_index(&vec![0; MAX_INDEX_BYTES]).is_ok());
    assert_eq!(
        signer
            .sign_index(&vec![0; MAX_INDEX_BYTES + 1])
            .unwrap_err()
            .category,
        "SigningLimit"
    );
    assert_eq!(
        Signer::open(&root.0).unwrap().sign_index(index).unwrap(),
        signer.sign_index(index).unwrap()
    );
    let fixture = Signer::from_seed([3; 32]);
    assert_eq!(fixture.key_id(), digest(&fixture.public_key()));
    assert_eq!(
        fixture.sign_index(index).unwrap(),
        Signer::from_seed([3; 32]).sign_index(index).unwrap()
    );
}

#[test]
fn configuration_capture_backup_and_restore_exclude_and_preserve_keys() {
    let root = Root::new();
    root.put("settings.json", &settings(100));
    let signer = test_signer(&root.0);
    let key_bytes = fs::read(active(&root.0)).unwrap();
    let seed = record(&root.0)["seed"].as_str().unwrap().to_owned();

    let observed = capture(&root.0).unwrap();
    assert_eq!(observed.files.keys().collect::<Vec<_>>(), ["settings.json"]);
    let receipt = backup::write(&root.0, observed.clone(), None).unwrap();
    let snapshot = backup::read(Path::new(&receipt.path)).unwrap();
    assert!(snapshot.files.keys().all(|path| !path.starts_with(KEYS)));
    let archive = fs::read(&receipt.path).unwrap();
    assert!(
        !archive
            .windows(seed.len())
            .any(|window| window == seed.as_bytes())
    );

    let replacement = Capture::from_files(
        BTreeMap::from([("settings.json".to_owned(), settings(200))]),
        true,
    )
    .unwrap();
    let plan = restore::prepare(
        replacement.clone(),
        observed.clone(),
        Some(&observed.generation),
    )
    .unwrap();
    restore::install(&root.0, plan).unwrap();
    assert_eq!(capture(&root.0).unwrap(), replacement);
    assert_eq!(fs::read(active(&root.0)).unwrap(), key_bytes);
    assert_eq!(Signer::open(&root.0).unwrap().key_id(), signer.key_id());
}
