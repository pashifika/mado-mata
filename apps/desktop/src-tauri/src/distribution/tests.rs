use super::*;
use crate::author_signing::MAX_INDEX_BYTES;
use crate::configuration::tests::Root;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use mado_runtime_comparison::images::{PACKAGE_BYTES, PACKAGE_NON_IMAGE_BYTES};
use std::io::Cursor;
use std::path::PathBuf;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const SOURCE: &str = "export function readiness(): MadoReady {\n  return \"Ready\";\n}\n\nexport function workflow(): void {\n  host.state.message = host.options.message;\n}\n";

type Entries = Vec<(String, Vec<u8>)>;

fn idle() -> AtomicBool {
    AtomicBool::new(false)
}

fn signer() -> Signer {
    Signer::from_seed([7; 32])
}

fn payload(bytes: Vec<u8>) -> PayloadBytes {
    PayloadBytes::new(bytes).unwrap()
}

/// A complete saved package; pretty printing proves raw bytes are kept, not reserialized.
fn files(release: &str) -> BTreeMap<String, PayloadBytes> {
    let manifest = json!({
        "version":1,"package_id":"sample","runtime":"typescript","sdk":"mado-host-v1","entry_contract":"ready-string-v1",
        "entries":{"readiness":{"module":"main.ts","function":"readiness"},"workflow":{"module":"main.ts","function":"workflow"}},
        "sources":["main.ts"],"schema":"schema.json","profiles":{"default":"profiles/default.json"},"assets":{},"source_maps":{},"dependencies":{},
        "distribution":{
            "version":1,"release_version":release,"name":"Sample","description":"Plain <b>text</b>",
            "author_name":"Author","license":"MIT",
            "game":{"id":"sample-game","names":{"en":"Sample Game","ja":"サンプル"},"aliases":["sample.exe"]},
            "tags":["internal"]
        }
    });
    let schema = json!({"version":1,"type":"object","additionalProperties":false,"required":["message"],"properties":{"message":{"type":"string","minLength":1,"maxLength":256,"default":"Hello"}}});
    let profile = json!({"package_id":"sample","schema_version":1,"options":{}});
    BTreeMap::from([
        (
            "package.json".to_owned(),
            payload(serde_json::to_vec_pretty(&manifest).unwrap()),
        ),
        (
            "schema.json".to_owned(),
            payload(serde_json::to_vec(&schema).unwrap()),
        ),
        (
            "profiles/default.json".to_owned(),
            payload(serde_json::to_vec(&profile).unwrap()),
        ),
        ("main.ts".to_owned(), payload(SOURCE.as_bytes().to_vec())),
    ])
}

fn draft(release: &str) -> PackageDraft {
    PackageDraft::from_files(files(release), &crate::authoring::limits().unwrap()).unwrap()
}

fn export(release: &str) -> PreparedPackage {
    prepare_export(draft(release), &signer(), &idle()).unwrap()
}

fn archive_of(prepared: &PreparedPackage) -> Vec<u8> {
    prepared.archive.to_vec()
}

/// Reads members with the ZIP library, independent of the package reader.
fn members(bytes: &[u8]) -> Entries {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();
    (0..zip.len())
        .map(|index| {
            let mut file = zip.by_index(index).unwrap();
            let mut body = Vec::new();
            file.read_to_end(&mut body).unwrap();
            (file.name().to_owned(), body)
        })
        .collect()
}

fn member(entries: &Entries, name: &str) -> Vec<u8> {
    entries
        .iter()
        .find(|(path, _)| path == name)
        .unwrap()
        .1
        .clone()
}

fn replaced(entries: &Entries, name: &str, body: Vec<u8>) -> Entries {
    entries
        .iter()
        .map(|(path, old)| {
            (
                path.clone(),
                if path == name {
                    body.clone()
                } else {
                    old.clone()
                },
            )
        })
        .collect()
}

fn without(entries: &Entries, name: &str) -> Entries {
    entries
        .iter()
        .filter(|(path, _)| path != name)
        .cloned()
        .collect()
}

fn with(entries: &Entries, name: &str, body: &[u8]) -> Entries {
    let mut entries = entries.clone();
    entries.push((name.to_owned(), body.to_vec()));
    entries
}

/// Replaces the signed index and signs it again with the test key.
fn resigned(entries: &Entries, index: &Value) -> Entries {
    let index = serde_json::to_vec(index).unwrap();
    let signer = signer();
    let signature = format::encode_signature(
        &signer.public_key(),
        signer.key_id(),
        &signer.sign_index(&index).unwrap(),
    )
    .unwrap();
    replaced(
        &replaced(entries, "index.json", index),
        "signature.json",
        signature,
    )
}

fn zip_of(entries: &Entries, method: CompressionMethod) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(method)
        .unix_permissions(0o644);
    for (name, body) in entries {
        writer.start_file(name.as_str(), options).unwrap();
        writer.write_all(body).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// Imports through a real operator-selected file, as the host does.
fn import(selection: &Root, bytes: &[u8]) -> Result<PreparedPackage, Fault> {
    let path = selection.0.join("selected.mmpkg");
    let _ = fs::remove_file(&path);
    fs::write(&path, bytes).unwrap();
    prepare_import(&path, &idle())
}

/// Central and local header offsets of a named member in an archive.
fn headers(bytes: &[u8], name: &str) -> (usize, usize) {
    let end = bytes.len() - 22;
    let count = usize::from(u16::from_le_bytes([bytes[end + 10], bytes[end + 11]]));
    let mut central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    for _ in 0..count {
        let field = |offset: usize| {
            usize::from(u16::from_le_bytes([
                bytes[central + offset],
                bytes[central + offset + 1],
            ]))
        };
        let (name_length, extra, comment) = (field(28), field(30), field(32));
        if &bytes[central + 46..central + 46 + name_length] == name.as_bytes() {
            let local = u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap());
            return (central, local as usize);
        }
        central += 46 + name_length + extra + comment;
    }
    panic!("member {name} is absent");
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Claims a different uncompressed size in both headers of one member.
fn declared_size(mut bytes: Vec<u8>, name: &str, size: u32) -> Vec<u8> {
    let (central, local) = headers(&bytes, name);
    put_u32(&mut bytes, central + 24, size);
    put_u32(&mut bytes, local + 22, size);
    bytes
}

fn names(directory: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(directory)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn snapshot(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_owned()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.push((path.clone(), Vec::new()));
                pending.push(path);
            } else {
                found.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    found.sort();
    found
}

/// Every path below `directory`, relative with `/` separators, sorted.
fn tree(directory: &Path) -> Vec<String> {
    let mut paths: Vec<_> = snapshot(directory)
        .into_iter()
        .map(|(path, _)| {
            path.strip_prefix(directory)
                .unwrap()
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect();
    paths.sort();
    paths
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// The fault-injection cases need an unprivileged owner whose modes are enforced.
#[cfg(unix)]
fn read_only(directory: &Path) {
    set_mode(directory, 0o500);
    let probe = fs::write(directory.join("permission-probe"), b"");
    assert_eq!(
        probe.unwrap_err().kind(),
        io::ErrorKind::PermissionDenied,
        "directory modes must be enforced for this regression"
    );
}

#[cfg(unix)]
fn rewrite(path: PathBuf, change: impl FnOnce(Vec<u8>) -> Vec<u8>) {
    let bytes = fs::read(&path).unwrap();
    // Rewriting an existing file keeps its private mode and single link.
    fs::write(&path, change(bytes)).unwrap();
}

#[test]
fn exported_release_installs_and_reverifies_through_every_consumer() {
    let data = Root::new();
    let output = Root::new();
    let prepared = export("1.0.0");
    let facts = prepared.facts().clone();
    let archive = archive_of(&prepared);
    assert_eq!(facts.package_id, "sample");
    assert_eq!(facts.release_version, "1.0.0");
    assert_eq!(facts.name, "Sample");
    assert_eq!(facts.description, "Plain <b>text</b>");
    assert_eq!(facts.game_names["ja"], "サンプル");
    assert_eq!(facts.key_id, signer().key_id());
    assert_eq!(facts.revision, draft("1.0.0").revision());
    assert_eq!(facts.artifact_id, format::sha256_hex(&archive));
    let total: usize = files("1.0.0").values().map(|bytes| bytes.len()).sum();
    assert_eq!((facts.files, facts.bytes), (4, total));
    // Exactly the envelope and the declared payload, holding the raw saved bytes.
    let entries = members(&archive);
    let listed: Vec<_> = entries.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        listed,
        [
            "index.json",
            "signature.json",
            "package/main.ts",
            "package/package.json",
            "package/profiles/default.json",
            "package/schema.json"
        ]
    );
    assert_eq!(
        member(&entries, "package/package.json"),
        files("1.0.0")["package.json"].to_vec()
    );
    // The fixed writer policy makes one capture produce one artifact.
    assert_eq!(archive_of(&export("1.0.0")), archive);

    let destination = output.0.join("sample.mmpkg");
    let published = publish_export(&prepared, &destination, &idle()).unwrap();
    assert!(published.committed && !published.unchanged && published.cleanup_error.is_none());
    assert_eq!(published.artifact_id, facts.artifact_id);
    assert_eq!(fs::read(&destination).unwrap(), archive);
    assert_eq!(names(&output.0), ["sample.mmpkg"]);

    let imported = prepare_import(&destination, &idle()).unwrap();
    assert_eq!(imported.facts(), &facts);
    // The review retains the exact captured bytes for installation.
    assert_eq!(&*imported.archive, archive.as_slice());
    assert_eq!(imported.draft().files(), prepared.draft().files());
    imported.draft().validate().unwrap();
    // Confirmation uses the captured artifact, never a reopened replacement.
    fs::write(&destination, b"replacement after review").unwrap();

    let file = installed_destination(&data.0, &facts).unwrap();
    assert_eq!(
        file,
        data.0
            .canonicalize()
            .unwrap()
            .join("pkgs")
            .join(&facts.key_id)
            .join("sample")
            .join(format!("{}.mmpkg", facts.artifact_id))
    );
    let installed = publish_import(&data.0, &imported, &idle()).unwrap();
    assert!(installed.committed && !installed.unchanged && installed.cleanup_error.is_none());
    assert_eq!(Path::new(&installed.path), file);
    // The installation is the original archive file alone: nothing is extracted and
    // no envelope copy or receipt is written beside it.
    assert_eq!(fs::read(&file).unwrap(), archive);
    let layout = [
        "pkgs".to_owned(),
        "pkgs/.import.lock".to_owned(),
        format!("pkgs/{}", facts.key_id),
        format!("pkgs/{}/sample", facts.key_id),
        format!("pkgs/{}/sample/{}.mmpkg", facts.key_id, facts.artifact_id),
    ];
    assert_eq!(tree(&data.0), layout);

    let verified = verify_installed(&data.0, "sample", &facts.artifact_id).unwrap();
    assert_eq!(verified.facts(), &facts);
    assert_eq!(verified.archive_path(), file);
    assert_eq!(verified.draft().files(), prepared.draft().files());
    verified.draft().validate().unwrap();

    let again = publish_import(&data.0, &imported, &idle()).unwrap();
    assert!(again.unchanged && !again.committed && again.cleanup_error.is_none());
    assert_eq!(Path::new(&again.path), file);
    assert_eq!(tree(&data.0), layout);

    let listed = installed_releases(&data.0).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].facts.as_ref(), Some(&facts));
    assert!(listed[0].fault.is_none());

    let identity = InstalledIdentity {
        package_id: "sample".into(),
        artifact_id: facts.artifact_id.clone(),
    };
    // The archive and any path beneath its name are never ordinary package locations.
    for path in [
        file.clone(),
        file.join("main.ts"),
        file.join("package/profiles/default.json"),
    ] {
        assert_eq!(
            managed_identity(&data.0, &path).unwrap(),
            Some(identity.clone()),
            "{path:?}"
        );
    }
    let package = file.parent().unwrap();
    for path in [
        package.to_owned(),
        package.parent().unwrap().to_owned(),
        data.0.join("pkgs"),
        output.0.clone(),
    ] {
        assert_eq!(managed_identity(&data.0, &path).unwrap(), None, "{path:?}");
    }
    assert_eq!(
        managed_identity(&output.0.join("absent"), &file).unwrap(),
        None
    );
}

#[test]
fn ordinary_pkgs_directories_are_never_adopted() {
    let data = Root::new();
    let plain = data.0.join("pkgs/plain");
    let digest_named = plain.join("a".repeat(64));
    fs::create_dir_all(&digest_named).unwrap();
    fs::write(plain.join("package.json"), b"{}").unwrap();
    fs::write(digest_named.join("main.ts"), b"export {};").unwrap();
    // A signer-shaped folder holding an ordinary package and a non-artifact archive name.
    let namespace = data.0.join("pkgs").join("b".repeat(64)).join("plain");
    fs::create_dir_all(&namespace).unwrap();
    fs::write(namespace.join("package.json"), b"{}").unwrap();
    fs::write(namespace.join("notes.mmpkg"), b"not an artifact").unwrap();
    assert!(installed_releases(&data.0).unwrap().is_empty());
    for path in [
        &plain,
        &digest_named,
        &namespace,
        &namespace.join("notes.mmpkg"),
    ] {
        assert_eq!(managed_identity(&data.0, path).unwrap(), None, "{path:?}");
    }
    assert_eq!(
        verify_installed(&data.0, "../plain", &"a".repeat(64))
            .unwrap_err()
            .category,
        "InstalledReference"
    );
    assert_eq!(
        verify_installed(&data.0, "plain", &"A".repeat(64))
            .unwrap_err()
            .category,
        "InstalledReference"
    );
    // An ordinary folder shaped like an artifact reference is never read as one.
    assert_eq!(
        verify_installed(&data.0, "plain", &"a".repeat(64))
            .unwrap_err()
            .category,
        "InstalledMissing"
    );
}

#[test]
fn independent_containers_are_admitted_and_release_conflicts_refuse() {
    let data = Root::new();
    let selection = Root::new();
    let stored = export("1.0.0");
    let entries = members(&archive_of(&stored));
    // Another standard writer: reversed order, deflate, and streamed data descriptors.
    let mut reversed = entries.clone();
    reversed.reverse();
    let deflated = zip_of(&reversed, CompressionMethod::Deflated);
    let mut streamed = ZipWriter::new_stream(Vec::new());
    for (name, body) in &entries {
        streamed
            .start_file(
                name.as_str(),
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .unwrap();
        streamed.write_all(body).unwrap();
    }
    let streamed = streamed.finish().unwrap().into_inner();
    for bytes in [&deflated, &streamed] {
        let admitted = import(&selection, bytes).unwrap();
        assert_eq!(admitted.draft().revision(), stored.draft().revision());
        assert_eq!(admitted.facts().artifact_id, format::sha256_hex(bytes));
        assert_ne!(admitted.facts().artifact_id, stored.facts().artifact_id);
    }

    let original = import(&selection, &archive_of(&stored)).unwrap();
    publish_import(&data.0, &original, &idle()).unwrap();
    let other_bytes = import(&selection, &deflated).unwrap();
    let rekeyed = prepare_export(draft("1.0.0"), &Signer::from_seed([9; 32]), &idle()).unwrap();
    let rekeyed = import(&selection, &archive_of(&rekeyed)).unwrap();
    for conflicting in [&other_bytes, &rekeyed] {
        let fault = publish_import(&data.0, conflicting, &idle()).unwrap_err();
        assert_eq!(fault.category, "PackageConflict", "{fault:?}");
        assert_eq!(
            fault.context["installed_artifact_id"],
            original.facts().artifact_id
        );
    }
    // A refused release creates nothing, not even its signer folder.
    let packages = data.0.join("pkgs");
    let key = signer().key_id().to_owned();
    assert_eq!(names(&packages), [".import.lock".to_owned(), key.clone()]);
    assert_eq!(
        names(&packages.join(&key).join("sample")),
        [format!("{}.mmpkg", original.facts().artifact_id)]
    );

    // Distinct release versions coexist, also from another signer; nothing chooses a
    // latest one.
    let next = import(&selection, &archive_of(&export("1.1.0"))).unwrap();
    publish_import(&data.0, &next, &idle()).unwrap();
    let foreign = prepare_export(draft("2.0.0"), &Signer::from_seed([9; 32]), &idle()).unwrap();
    let foreign = import(&selection, &archive_of(&foreign)).unwrap();
    assert_ne!(foreign.facts().key_id, key);
    let published = publish_import(&data.0, &foreign, &idle()).unwrap();
    assert_eq!(
        Path::new(&published.path),
        installed_destination(&data.0, foreign.facts()).unwrap()
    );
    let listed = installed_releases(&data.0).unwrap();
    assert_eq!(listed.len(), 3);
    assert!(listed.iter().all(|release| release.fault.is_none()));
    for release in [&original, &next, &foreign] {
        verify_installed(&data.0, "sample", &release.facts().artifact_id).unwrap();
    }
}

#[test]
fn changed_metadata_source_index_or_signature_refuses_at_import() {
    let selection = Root::new();
    let entries = members(&archive_of(&export("1.0.0")));
    let index: Value = serde_json::from_slice(&member(&entries, "index.json")).unwrap();
    let signature: Value = serde_json::from_slice(&member(&entries, "signature.json")).unwrap();
    let signature_with = |name: &str, value: Value| {
        let mut changed = signature.clone();
        changed[name] = value;
        replaced(
            &entries,
            "signature.json",
            serde_json::to_vec(&changed).unwrap(),
        )
    };
    let mut source = member(&entries, "package/main.ts");
    source[0] ^= 1;
    let mut manifest: Value =
        serde_json::from_slice(&member(&entries, "package/package.json")).unwrap();
    manifest["distribution"]["name"] = json!("Renamed");
    let mut forged = STANDARD
        .decode(signature["signature"].as_str().unwrap())
        .unwrap();
    forged[0] ^= 1;
    let other = Signer::from_seed([9; 32]);
    let foreign = {
        let mut changed = signature.clone();
        changed["public_key"] = json!(STANDARD.encode(other.public_key()));
        changed["key_id"] = json!(other.key_id());
        replaced(
            &entries,
            "signature.json",
            serde_json::to_vec(&changed).unwrap(),
        )
    };
    let original_signature = member(&entries, "signature.json");
    let duplicate = [b"{\"format\":1,".as_slice(), &original_signature[1..]].concat();
    let fact = |field: &str, value: Value| {
        let mut changed = index.clone();
        let main = changed["files"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|file| file["path"] == "main.ts")
            .unwrap();
        main[field] = value;
        resigned(&entries, &changed)
    };
    let mut unsorted = index.clone();
    unsorted["files"].as_array_mut().unwrap().reverse();
    let mut repeated = index.clone();
    let first = repeated["files"][0].clone();
    repeated["files"].as_array_mut().unwrap().insert(0, first);
    let mut unknown = index.clone();
    unknown["release_version"] = json!("1.0.0");
    let cases: Vec<(&str, Entries, &str)> = vec![
        (
            "source byte",
            replaced(&entries, "package/main.ts", source),
            "PackageIntegrity",
        ),
        (
            "metadata",
            replaced(
                &entries,
                "package/package.json",
                serde_json::to_vec(&manifest).unwrap(),
            ),
            "PackageIntegrity",
        ),
        (
            "reserialized index",
            replaced(
                &entries,
                "index.json",
                serde_json::to_vec_pretty(&index).unwrap(),
            ),
            "PackageSignature",
        ),
        (
            "algorithm",
            signature_with("algorithm", json!("RSA")),
            "PackageSignature",
        ),
        (
            "fingerprint",
            signature_with("key_id", json!(other.key_id())),
            "PackageSignature",
        ),
        ("foreign key", foreign, "PackageSignature"),
        (
            "signature bytes",
            signature_with("signature", json!(STANDARD.encode(&forged))),
            "PackageSignature",
        ),
        (
            "unpadded key",
            signature_with(
                "public_key",
                json!(
                    signature["public_key"]
                        .as_str()
                        .unwrap()
                        .trim_end_matches('=')
                ),
            ),
            "PackageSignature",
        ),
        (
            "signature member",
            signature_with("note", json!("unsigned")),
            "PackageSignature",
        ),
        (
            "duplicate signature member",
            replaced(&entries, "signature.json", duplicate),
            "PackageSignature",
        ),
        (
            "signed wrong digest",
            fact("sha256", json!("0".repeat(64))),
            "PackageIntegrity",
        ),
        (
            "signed wrong length",
            fact("bytes", json!(1)),
            "PackageIntegrity",
        ),
        (
            "unsorted index",
            resigned(&entries, &unsorted),
            "PackageFormat",
        ),
        (
            "repeated index file",
            resigned(&entries, &repeated),
            "PackageFormat",
        ),
        (
            "unsigned release metadata",
            resigned(&entries, &unknown),
            "PackageFormat",
        ),
        (
            "missing signature",
            without(&entries, "signature.json"),
            "PackageFormat",
        ),
        (
            "missing index",
            without(&entries, "index.json"),
            "PackageFormat",
        ),
        (
            "missing payload",
            without(&entries, "package/schema.json"),
            "PackageIntegrity",
        ),
        (
            "extra payload",
            with(&entries, "package/extra.ts", b"export {};"),
            "PackageIntegrity",
        ),
        (
            "unlisted member",
            with(&entries, "notes.txt", b"unsigned"),
            "PackageFormat",
        ),
        (
            "OS metadata",
            with(&entries, "package/Thumbs.db", b"cache"),
            "PackageFormat",
        ),
        (
            "unsafe path",
            with(&entries, "package/../escape.ts", b"export {};"),
            "PackageFormat",
        ),
        (
            "case alias",
            with(&entries, "package/MAIN.ts", SOURCE.as_bytes()),
            "PackageFormat",
        ),
    ];
    for (label, entries, category) in cases {
        for method in [CompressionMethod::Stored, CompressionMethod::Deflated] {
            let fault = import(&selection, &zip_of(&entries, method)).unwrap_err();
            assert_eq!(fault.category, category, "{label}: {fault:?}");
        }
    }
}

#[test]
fn unsupported_truncated_and_ambiguous_containers_refuse() {
    let selection = Root::new();
    let valid = archive_of(&export("1.0.0"));
    let entries = members(&valid);
    let mut cases: Vec<(&str, Vec<u8>)> = [0, 21, valid.len() / 2, valid.len() - 1]
        .into_iter()
        .map(|length| ("truncated", valid[..length].to_vec()))
        .collect();
    cases.push(("prefixed", [b"MZ".as_slice(), valid.as_slice()].concat()));
    cases.push(("trailing", [valid.as_slice(), b"x".as_slice()].concat()));
    cases.push((
        "legacy CustomZip",
        b"MMCZ\x01legacy encrypted payload".to_vec(),
    ));
    let mut commented = valid.clone();
    let length = commented.len();
    put_u16(&mut commented, length - 2, 4);
    commented.extend_from_slice(b"note");
    cases.push(("archive comment", commented));
    let mut encrypted = valid.clone();
    let (central, local) = headers(&encrypted, "package/main.ts");
    put_u16(&mut encrypted, central + 8, 1);
    put_u16(&mut encrypted, local + 6, 1);
    cases.push(("encrypted", encrypted));
    let mut special = valid.clone();
    let (central, _) = headers(&special, "package/main.ts");
    put_u32(&mut special, central + 38, 0o010_644 << 16);
    cases.push(("special file", special));
    let mut duplicate = zip_of(
        &with(&entries, "package/maiX.ts", SOURCE.as_bytes()),
        CompressionMethod::Stored,
    );
    let (needle, name) = (b"package/maiX.ts".as_slice(), b"package/main.ts".as_slice());
    for start in 0..duplicate.len() - needle.len() {
        if &duplicate[start..start + needle.len()] == needle {
            duplicate[start..start + needle.len()].copy_from_slice(name);
        }
    }
    cases.push(("duplicate", duplicate));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut directory = ZipWriter::new(Cursor::new(Vec::new()));
    let mut link = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in &entries {
        for writer in [&mut directory, &mut link] {
            writer.start_file(name.as_str(), options).unwrap();
            writer.write_all(body).unwrap();
        }
    }
    directory
        .add_directory("package/profiles/", options)
        .unwrap();
    link.add_symlink("package/link.ts", "main.ts", options)
        .unwrap();
    cases.push(("directory entry", directory.finish().unwrap().into_inner()));
    cases.push(("link", link.finish().unwrap().into_inner()));
    for (label, bytes) in cases {
        let fault = import(&selection, &bytes).unwrap_err();
        assert_eq!(fault.category, "PackageFormat", "{label}: {fault:?}");
    }
}

#[test]
fn envelope_and_decompression_bounds_refuse_before_admission() {
    let selection = Root::new();
    let entries = members(&archive_of(&export("1.0.0")));
    let index: Value = serde_json::from_slice(&member(&entries, "index.json")).unwrap();
    // Deflate output beyond the signed and declared length stops at that length.
    let mut short = index.clone();
    for file in short["files"].as_array_mut().unwrap() {
        if file["path"] == "main.ts" {
            file["bytes"] = json!(10);
        }
    }
    let bomb = declared_size(
        zip_of(&resigned(&entries, &short), CompressionMethod::Deflated),
        "package/main.ts",
        10,
    );
    let fault = import(&selection, &bomb).unwrap_err();
    assert_eq!(fault.category, "PackageFormat", "{fault:?}");
    assert_eq!(fault.context["path"], "package/main.ts");

    // Declared payload beyond the package total refuses from the directory alone.
    let total = declared_size(
        zip_of(&entries, CompressionMethod::Deflated),
        "package/main.ts",
        u32::try_from(PACKAGE_BYTES + 1).unwrap(),
    );
    assert_eq!(
        import(&selection, &total).unwrap_err().category,
        "PackageLimit"
    );

    for (name, size) in [
        ("index.json", MAX_INDEX_BYTES + 1),
        ("signature.json", MAX_SIGNATURE_BYTES + 1),
    ] {
        let bloated = replaced(&entries, name, vec![b' '; size]);
        let fault = import(&selection, &zip_of(&bloated, CompressionMethod::Stored)).unwrap_err();
        assert_eq!(fault.category, "PackageLimit", "{name}: {fault:?}");
    }

    let mut crowded = entries.clone();
    for index in 0..=MAX_PAYLOAD_FILES {
        crowded.push((format!("package/extra{index}.ts"), b"export {};".to_vec()));
    }
    assert_eq!(
        import(&selection, &zip_of(&crowded, CompressionMethod::Stored))
            .unwrap_err()
            .category,
        "PackageLimit"
    );

    let sparse = selection.0.join("sparse.mmpkg");
    File::create(&sparse)
        .unwrap()
        .set_len(MAX_ARCHIVE_BYTES as u64 + 1)
        .unwrap();
    assert_eq!(
        prepare_import(&sparse, &idle()).unwrap_err().category,
        "PackageLimit"
    );
}

/// Sets the reserved Deflate block type in a member's first compressed byte: decoding
/// that member refuses at once, while headers, sizes, CRC, index and signature stay valid.
fn poisoned(mut bytes: Vec<u8>, name: &str) -> Vec<u8> {
    let (_, local) = headers(&bytes, name);
    let length = |offset: usize| {
        usize::from(u16::from_le_bytes([
            bytes[local + offset],
            bytes[local + offset + 1],
        ]))
    };
    let data = local + 30 + length(26) + length(28);
    bytes[data] |= 0x06;
    bytes
}

#[test]
fn review_payload_non_image_budget_refuses_before_decompression() {
    let selection = Root::new();
    // Independently built and signed Deflate archives; export refuses oversized payloads.
    let signed = |contents: &BTreeMap<String, PayloadBytes>| {
        let index: Value =
            serde_json::from_slice(&format::encode_index(contents).unwrap()).unwrap();
        let mut entries: Entries = vec![
            ("index.json".into(), Vec::new()),
            ("signature.json".into(), Vec::new()),
        ];
        for (path, bytes) in contents {
            entries.push((format!("package/{path}"), bytes.to_vec()));
        }
        zip_of(&resigned(&entries, &index), CompressionMethod::Deflated)
    };
    import(&selection, &signed(&files("1.0.0"))).unwrap();
    let (whole, half) = (PACKAGE_NON_IMAGE_BYTES + 1, PACKAGE_NON_IMAGE_BYTES / 2);
    // Aggregate files fit individually but exceed the shared non-image bound.
    let cases: [(&str, &[(&str, usize)]); 3] = [
        ("main.ts", &[("main.ts", whole)]),
        ("package.json", &[("package.json", whole)]),
        ("schema.json", &[("main.ts", half), ("schema.json", half)]),
    ];
    for (path, changes) in cases {
        let name = format!("package/{path}");
        let control = import(&selection, &poisoned(signed(&files("1.0.0")), &name)).unwrap_err();
        assert_eq!(control.category, "PackageFormat", "{path}: {control:?}");
        assert_eq!(control.context["path"], name, "{path}: {control:?}");
        let mut oversized = files("1.0.0");
        for &(changed, length) in changes {
            let mut bytes = oversized[changed].to_vec();
            bytes.resize(length, b' ');
            oversized.insert(changed.into(), payload(bytes));
        }
        let limit = import(&selection, &signed(&oversized)).unwrap_err();
        assert_ne!(limit.category, control.category, "{path}: {limit:?}");
        let fault = import(&selection, &poisoned(signed(&oversized), &name)).unwrap_err();
        assert_eq!(fault.category, limit.category, "{path}: {fault:?}");
    }
}

#[test]
fn review_payload_large_images_keep_their_allowance() {
    let selection = Root::new();
    let mut package = files("1.0.0");
    let path = "assets/large.data";
    let mut manifest: Value = serde_json::from_slice(&package["package.json"]).unwrap();
    manifest["assets"]["large"] =
        json!({"path":path,"format":"raw-rgba8","width":513,"height":513});
    package.insert(
        "package.json".into(),
        payload(serde_json::to_vec(&manifest).unwrap()),
    );
    package.insert(path.into(), payload(vec![0; 513 * 513 * 4]));
    let draft = PackageDraft::from_files(package, &crate::authoring::limits().unwrap()).unwrap();
    let prepared = prepare_export(draft, &signer(), &idle()).unwrap();
    let entries = members(&archive_of(&prepared));
    let deflated = zip_of(&entries, CompressionMethod::Deflated);
    let imported = import(&selection, &deflated).unwrap();
    assert_eq!(imported.draft().revision(), prepared.draft().revision());
}

#[test]
fn cancellation_and_missing_metadata_stop_preparation() {
    let selection = Root::new();
    let cancelled_flag = AtomicBool::new(true);
    assert_eq!(
        prepare_export(draft("1.0.0"), &signer(), &cancelled_flag)
            .unwrap_err()
            .category,
        "PackageCancelled"
    );
    let archive = archive_of(&export("1.0.0"));
    let path = selection.0.join("selected.mmpkg");
    fs::write(&path, &archive).unwrap();
    assert_eq!(
        prepare_import(&path, &cancelled_flag).unwrap_err().category,
        "PackageCancelled"
    );

    let mut plain = files("1.0.0");
    let mut manifest: Value = serde_json::from_slice(&plain["package.json"]).unwrap();
    manifest.as_object_mut().unwrap().remove("distribution");
    plain.insert(
        "package.json".into(),
        payload(serde_json::to_vec(&manifest).unwrap()),
    );
    let plain = PackageDraft::from_files(plain, &crate::authoring::limits().unwrap()).unwrap();
    assert_eq!(
        prepare_export(plain, &signer(), &idle())
            .unwrap_err()
            .category,
        "PackageMetadata"
    );
}

#[cfg(unix)]
#[test]
fn export_publication_never_replaces_and_reports_committed_truth() {
    let output = Root::new();
    let prepared = export("1.0.0");
    let archive = archive_of(&prepared);
    let destination = output.0.join("sample.mmpkg");

    fs::write(&destination, b"operator file").unwrap();
    let fault = publish_export(&prepared, &destination, &idle()).unwrap_err();
    assert_eq!(fault.category, "PackageDestinationExists");
    assert_eq!(fs::read(&destination).unwrap(), b"operator file");
    assert_eq!(names(&output.0), ["sample.mmpkg"]);
    fs::remove_file(&destination).unwrap();

    assert_eq!(
        publish_export(&prepared, &destination, &AtomicBool::new(true))
            .unwrap_err()
            .category,
        "PackageCancelled"
    );
    let cancel = idle();
    let fault = publish_export_with(&prepared, &destination, &cancel, &mut |stage, _| {
        if stage == Stage::Staged {
            cancel.store(true, Ordering::Release);
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(fault.category, "PackageCancelled");
    assert!(fault.context.get("cleanup_error").is_none());
    assert!(names(&output.0).is_empty());

    // A file that appears before the rename wins; the staging copy is removed.
    let fault = publish_export_with(&prepared, &destination, &idle(), &mut |stage, path| {
        if stage == Stage::Publish {
            fs::write(path, b"racing writer").unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(fault.category, "PackageDestinationExists");
    assert_eq!(fs::read(&destination).unwrap(), b"racing writer");
    assert_eq!(names(&output.0), ["sample.mmpkg"]);
    fs::remove_file(&destination).unwrap();

    read_only(&output.0);
    let fault = publish_export(&prepared, &destination, &idle()).unwrap_err();
    set_mode(&output.0, 0o700);
    assert_eq!(fault.category, "ConfigurationIo", "{fault:?}");
    assert!(names(&output.0).is_empty());

    // Failed removal of refused staging is reported and the evidence retained.
    let cancel = idle();
    let fault = publish_export_with(&prepared, &destination, &cancel, &mut |stage, _| {
        if stage == Stage::Staged {
            set_mode(&output.0, 0o500);
            cancel.store(true, Ordering::Release);
        }
        Ok(())
    })
    .unwrap_err();
    set_mode(&output.0, 0o700);
    assert_eq!(fault.category, "PackageCancelled");
    assert_eq!(fault.context["cleanup_error"]["category"], "PackageStorage");
    let retained = names(&output.0);
    assert_eq!(retained.len(), 1);
    assert!(retained[0].starts_with(".mmpkg-export-"));
    assert!(!destination.exists());
    fs::remove_file(output.0.join(&retained[0])).unwrap();

    // A durable artifact whose finishing work fails is committed, not rolled back.
    let published = publish_export_with(&prepared, &destination, &idle(), &mut |stage, _| {
        if stage == Stage::Committed {
            set_mode(&output.0, 0o300);
        }
        Ok(())
    })
    .unwrap();
    set_mode(&output.0, 0o700);
    assert!(published.committed && !published.unchanged);
    assert_eq!(published.cleanup_error.unwrap().category, "ConfigurationIo");
    assert_eq!(fs::read(&destination).unwrap(), archive);
    let imported = prepare_import(&destination, &idle()).unwrap();
    assert_eq!(imported.facts(), prepared.facts());
    assert_eq!(
        publish_export(&imported, &output.0.join("copy.mmpkg"), &idle())
            .unwrap_err()
            .category,
        "PackagePrepared"
    );
}

#[cfg(unix)]
#[test]
fn import_publication_is_exclusive_atomic_and_reports_committed_truth() {
    let data = Root::new();
    let selection = Root::new();
    let archive = archive_of(&export("1.0.0"));
    let imported = import(&selection, &archive).unwrap();
    let artifact = imported.facts().artifact_id.clone();
    let destination = installed_destination(&data.0, imported.facts()).unwrap();
    let file_name = format!("{artifact}.mmpkg");
    let package = destination.parent().unwrap().to_owned();
    let signer_folder = package.parent().unwrap().to_owned();

    // An ordinary editable package or file at the release location is preserved, not
    // adopted.
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("package.json"), b"{}").unwrap();
    assert_eq!(
        publish_import(&data.0, &imported, &idle())
            .unwrap_err()
            .category,
        "PackageConflict"
    );
    assert_eq!(names(&package), ["package.json"]);
    fs::remove_dir_all(&signer_folder).unwrap();
    fs::write(&signer_folder, b"operator file").unwrap();
    assert_eq!(
        publish_import(&data.0, &imported, &idle())
            .unwrap_err()
            .category,
        "PackageConflict"
    );
    assert_eq!(fs::read(&signer_folder).unwrap(), b"operator file");
    fs::remove_file(&signer_folder).unwrap();
    fs::write(data.0.join("pkgs/package.json"), b"{}").unwrap();
    assert_eq!(
        publish_import(&data.0, &imported, &idle())
            .unwrap_err()
            .category,
        "PackageConflict"
    );
    assert!(!signer_folder.exists());
    fs::remove_file(data.0.join("pkgs/package.json")).unwrap();

    // Another importer holding the data-root lock excludes this one.
    let holder = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(data.0.join("pkgs/.import.lock"))
        .unwrap();
    holder.try_lock().unwrap();
    assert_eq!(
        publish_import(&data.0, &imported, &idle())
            .unwrap_err()
            .category,
        "PackageBusy"
    );
    drop(holder);

    assert_eq!(
        publish_import(&data.0, &imported, &AtomicBool::new(true))
            .unwrap_err()
            .category,
        "PackageCancelled"
    );
    let cancel = idle();
    let fault = installed::publish(&data.0, &imported, &cancel, &mut |stage, path| {
        if stage == Stage::Staged {
            // The staged installation is the captured archive file itself.
            assert!(fs::symlink_metadata(path).unwrap().is_file());
            assert_eq!(fs::read(path).unwrap(), archive);
            cancel.store(true, Ordering::Release);
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(fault.category, "PackageCancelled");
    assert!(fault.context.get("cleanup_error").is_none());
    assert!(names(&package).is_empty());
    assert!(installed_releases(&data.0).unwrap().is_empty());

    read_only(&package);
    let fault = publish_import(&data.0, &imported, &idle()).unwrap_err();
    set_mode(&package, 0o700);
    assert_eq!(fault.category, "ConfigurationIo", "{fault:?}");
    assert!(names(&package).is_empty());

    // A file that appears before the rename wins; the staging copy is removed.
    let fault = installed::publish(&data.0, &imported, &idle(), &mut |stage, path| {
        if stage == Stage::Publish {
            fs::write(path, b"racing writer").unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(fault.category, "PackageConflict");
    assert_eq!(names(&package), [file_name.clone()]);
    assert_eq!(fs::read(&destination).unwrap(), b"racing writer");
    fs::remove_file(&destination).unwrap();

    // Failed removal of refused staging is reported and the evidence retained.
    let cancel = idle();
    let fault = installed::publish(&data.0, &imported, &cancel, &mut |stage, _| {
        if stage == Stage::Staged {
            set_mode(&package, 0o500);
            cancel.store(true, Ordering::Release);
        }
        Ok(())
    })
    .unwrap_err();
    set_mode(&package, 0o700);
    assert_eq!(fault.category, "PackageCancelled");
    assert_eq!(fault.context["cleanup_error"]["category"], "PackageStorage");
    let retained = names(&package);
    assert_eq!(retained.len(), 1);
    assert!(retained[0].starts_with(".install-"));
    // Retained staging is neither listed nor read as the installation.
    assert!(installed_releases(&data.0).unwrap().is_empty());
    assert_eq!(
        verify_installed(&data.0, "sample", &artifact)
            .unwrap_err()
            .category,
        "InstalledMissing"
    );

    // A durable artifact whose finishing work fails is committed, not rolled back.
    let published = installed::publish(&data.0, &imported, &idle(), &mut |stage, _| {
        if stage == Stage::Committed {
            set_mode(&package, 0o300);
        }
        Ok(())
    })
    .unwrap();
    set_mode(&package, 0o700);
    assert!(published.committed && !published.unchanged);
    assert_eq!(published.cleanup_error.unwrap().category, "ConfigurationIo");
    assert_eq!(names(&package), [retained[0].clone(), file_name]);
    assert_eq!(fs::read(&destination).unwrap(), archive);
    assert_eq!(
        verify_installed(&data.0, "sample", &artifact)
            .unwrap()
            .facts(),
        imported.facts()
    );
    let again = publish_import(&data.0, &imported, &idle()).unwrap();
    assert!(again.unchanged && !again.committed);
}

#[cfg(unix)]
#[test]
fn installed_tampering_refuses_and_reimport_never_repairs() {
    let selection = Root::new();
    let archive = archive_of(&export("1.0.0"));
    let cases: [(&str, fn(&Path, &Path), &str); 8] = [
        (
            "archive byte",
            |file: &Path, _: &Path| {
                rewrite(file.to_owned(), |mut bytes| {
                    let middle = bytes.len() / 2;
                    bytes[middle] ^= 1;
                    bytes
                });
            },
            "InstalledIntegrity",
        ),
        (
            "appended byte",
            |file: &Path, _: &Path| {
                rewrite(file.to_owned(), |mut bytes| {
                    bytes.push(0);
                    bytes
                });
            },
            "InstalledIntegrity",
        ),
        (
            "truncated",
            |file: &Path, _: &Path| {
                rewrite(file.to_owned(), |mut bytes| {
                    bytes.truncate(bytes.len() / 2);
                    bytes
                });
            },
            "InstalledIntegrity",
        ),
        (
            "readable by others",
            |file: &Path, _: &Path| set_mode(file, 0o644),
            "InstalledIntegrity",
        ),
        (
            "hard link",
            |file: &Path, spare: &Path| fs::hard_link(file, spare.join("alias.mmpkg")).unwrap(),
            "InstalledIntegrity",
        ),
        (
            "symbolic link",
            |file: &Path, spare: &Path| {
                let moved = spare.join("moved.mmpkg");
                fs::rename(file, &moved).unwrap();
                std::os::unix::fs::symlink(&moved, file).unwrap();
            },
            "InstalledIntegrity",
        ),
        (
            "directory",
            |file: &Path, _: &Path| {
                fs::remove_file(file).unwrap();
                fs::create_dir(file).unwrap();
            },
            "InstalledIntegrity",
        ),
        (
            "missing",
            |file: &Path, _: &Path| fs::remove_file(file).unwrap(),
            "InstalledMissing",
        ),
    ];
    for (label, tamper, category) in cases {
        let data = Root::new();
        let spare = Root::new();
        let imported = import(&selection, &archive).unwrap();
        publish_import(&data.0, &imported, &idle()).unwrap();
        let artifact = imported.facts().artifact_id.clone();
        let file = installed_destination(&data.0, imported.facts()).unwrap();
        let package = file.parent().unwrap().to_owned();
        tamper(&file, &spare.0);
        let fault = verify_installed(&data.0, "sample", &artifact).unwrap_err();
        assert_eq!(fault.category, category, "{label}: {fault:?}");
        assert_eq!(fault.context["artifact_id"], artifact.as_str(), "{label}");
        let listed = installed_releases(&data.0).unwrap();
        if category == "InstalledMissing" {
            assert_eq!(fault.context["reimport_required"], true, "{label}");
            assert!(listed.is_empty(), "{label}");
            // Only the original archive restores a missing installation.
            let restored = publish_import(&data.0, &imported, &idle()).unwrap();
            assert!(restored.committed, "{label}");
            verify_installed(&data.0, "sample", &artifact).unwrap();
            continue;
        }
        assert_eq!(listed.len(), 1, "{label}");
        assert!(listed[0].facts.is_none(), "{label}");
        assert_eq!(
            listed[0].fault.as_ref().unwrap().category,
            category,
            "{label}"
        );
        let before = snapshot(&package);
        let fault = publish_import(&data.0, &imported, &idle()).unwrap_err();
        assert_eq!(fault.category, "InstalledIntegrity", "{label}: {fault:?}");
        assert_eq!(snapshot(&package), before, "{label}");
    }

    // An unreadable archive refuses as a storage failure, not as missing, and verifies
    // again once readable; nothing repairs or replaces it.
    let data = Root::new();
    let imported = import(&selection, &archive).unwrap();
    publish_import(&data.0, &imported, &idle()).unwrap();
    let artifact = imported.facts().artifact_id.clone();
    let file = installed_destination(&data.0, imported.facts()).unwrap();
    set_mode(&file, 0o200);
    assert_eq!(
        File::open(&file).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied,
        "file modes must be enforced for this regression"
    );
    let fault = verify_installed(&data.0, "sample", &artifact).unwrap_err();
    set_mode(&file, 0o600);
    assert_eq!(fault.category, "InstalledIntegrity", "{fault:?}");
    assert_eq!(fault.context["cause"]["category"], "PackageStorage");
    assert_eq!(
        verify_installed(&data.0, "sample", &artifact)
            .unwrap()
            .facts(),
        imported.facts()
    );
}

#[test]
fn installed_archives_bind_signer_folder_package_and_one_location() {
    let data = Root::new();
    let selection = Root::new();
    let imported = import(&selection, &archive_of(&export("1.0.0"))).unwrap();
    publish_import(&data.0, &imported, &idle()).unwrap();
    let artifact = imported.facts().artifact_id.clone();
    let file = installed_destination(&data.0, imported.facts()).unwrap();
    let package = file.parent().unwrap().to_owned();
    let signer_folder = package.parent().unwrap().to_owned();
    let foreign = signer_folder.with_file_name("b".repeat(64));
    let copy = foreign.join("sample").join(file.file_name().unwrap());

    // A second location for one artifact is listed once, with one attributable fault,
    // and neither location is chosen.
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    fs::copy(&file, &copy).unwrap();
    let fault = verify_installed(&data.0, "sample", &artifact).unwrap_err();
    assert_eq!(fault.category, "InstalledIntegrity", "{fault:?}");
    assert_eq!(fault.context["key_ids"].as_array().unwrap().len(), 2);
    let listed = installed_releases(&data.0).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (
            listed[0].package_id.as_str(),
            listed[0].artifact_id.as_str()
        ),
        ("sample", artifact.as_str())
    );
    assert_eq!(
        listed[0].fault.as_ref().unwrap().category,
        "InstalledIntegrity"
    );
    let fault = publish_import(&data.0, &imported, &idle()).unwrap_err();
    assert_eq!(fault.category, "PackageConflict", "{fault:?}");
    assert_eq!(fs::read(&copy).unwrap(), fs::read(&file).unwrap());

    // Only the signer folder named by the signed key verifies the archive.
    fs::remove_file(&file).unwrap();
    let fault = verify_installed(&data.0, "sample", &artifact).unwrap_err();
    assert_eq!(fault.category, "InstalledIntegrity", "{fault:?}");
    assert_eq!(fault.context["key_id"], "b".repeat(64));
    assert!(installed_releases(&data.0).unwrap()[0].fault.is_some());
    fs::remove_dir_all(&foreign).unwrap();

    // The package folder is bound to the package the archive names.
    assert!(
        publish_import(&data.0, &imported, &idle())
            .unwrap()
            .committed
    );
    verify_installed(&data.0, "sample", &artifact).unwrap();
    fs::rename(&package, signer_folder.join("renamed")).unwrap();
    assert_eq!(
        verify_installed(&data.0, "sample", &artifact)
            .unwrap_err()
            .category,
        "InstalledMissing"
    );
    let fault = verify_installed(&data.0, "renamed", &artifact).unwrap_err();
    assert_eq!(fault.category, "InstalledIntegrity", "{fault:?}");
    let listed = installed_releases(&data.0).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].package_id, "renamed");
    assert!(listed[0].fault.is_some());
}

#[test]
fn expanded_installations_stay_unavailable_until_original_reimport() {
    let data = Root::new();
    let selection = Root::new();
    let archive = archive_of(&export("1.0.0"));
    let imported = import(&selection, &archive).unwrap();
    let artifact = imported.facts().artifact_id.clone();
    // A container written by the superseded expanded layout.
    let relative = format!("pkgs/sample/{artifact}");
    for (name, body) in members(&archive) {
        data.put(&format!("{relative}/{name}"), &body);
    }
    data.put(&format!("{relative}/receipt.json"), b"{}");
    let container = data.0.join(&relative);
    let before = snapshot(&container);
    let identity = InstalledIdentity {
        package_id: "sample".into(),
        artifact_id: artifact.clone(),
    };

    // It is never read, listed or run; its reference asks for the original archive.
    let fault = verify_installed(&data.0, "sample", &artifact).unwrap_err();
    assert_eq!(fault.category, "InstalledMissing", "{fault:?}");
    assert_eq!(fault.context["reimport_required"], true);
    assert!(installed_releases(&data.0).unwrap().is_empty());
    // Its paths, including new ones, stay refused as ordinary packages.
    for path in [
        container.clone(),
        container.join("package/main.ts"),
        container.join("package/new.ts"),
    ] {
        assert_eq!(
            managed_identity(&data.0, &path).unwrap(),
            Some(identity.clone()),
            "{path:?}"
        );
    }

    // Reimporting the original archive restores the same identity beside it; the old
    // container is neither rebuilt nor removed.
    let restored = publish_import(&data.0, &imported, &idle()).unwrap();
    assert!(restored.committed && restored.cleanup_error.is_none());
    let verified = verify_installed(&data.0, "sample", &artifact).unwrap();
    assert_eq!(verified.facts(), imported.facts());
    assert_eq!(Path::new(&restored.path), verified.archive_path());
    assert_eq!(snapshot(&container), before);
    assert_eq!(
        managed_identity(&data.0, &container).unwrap(),
        Some(identity)
    );
}

/// One independently built and signed release of `package_id`, derived from the
/// sample package.
fn capacity_archive(signer: &Signer, package_id: &str, release: &str) -> Vec<u8> {
    let mut contents = files(release);
    for path in ["package.json", "profiles/default.json"] {
        let mut value: Value = serde_json::from_slice(&contents[path]).unwrap();
        value["package_id"] = json!(package_id);
        contents.insert(
            path.to_owned(),
            payload(serde_json::to_vec_pretty(&value).unwrap()),
        );
    }
    let draft = PackageDraft::from_files(contents, &crate::authoring::limits().unwrap()).unwrap();
    archive_of(&prepare_export(draft, signer, &idle()).unwrap())
}

/// The catalog lists at most 256 installed releases, so at that capacity a new import
/// refuses before anything is written, whether the catalog holds one package's
/// releases or releases spread over packages and signers; a verified exact reimport
/// stays unchanged and the catalog stays listable.
#[test]
fn review_capacity_full_catalog_refuses_new_imports_and_keeps_reimport() {
    let data = Root::new();
    let selection = Root::new();
    let packages = data.0.join("pkgs");
    let (local, foreign) = (signer(), Signer::from_seed([9; 32]));
    let attempt = |signer: &Signer, package_id: &str, release: &str| {
        import(&selection, &capacity_archive(signer, package_id, release)).unwrap()
    };
    // Places a release where a completed import leaves it, with the same private
    // modes, without one publication call per catalog entry.
    let place = |signer: &Signer, package_id: &str, release: &str| {
        let archive = capacity_archive(signer, package_id, release);
        let relative = format!(
            "pkgs/{}/{package_id}/{}.mmpkg",
            signer.key_id(),
            format::sha256_hex(&archive)
        );
        data.put(&relative, &archive);
        relative
    };
    let listed = || {
        let catalog = installed_releases(&data.0).unwrap();
        assert_eq!(catalog.len(), 256);
        let faults: Vec<_> = catalog
            .iter()
            .filter_map(|release| release.fault.as_ref())
            .collect();
        assert!(faults.is_empty(), "{faults:?}");
        catalog
    };
    let refuses = |prepared: &PreparedPackage| {
        let before = snapshot(&packages);
        let outcome = publish_import(&data.0, prepared, &idle());
        let facts = prepared.facts();
        let release = format!("{} {}", facts.package_id, facts.release_version);
        assert_eq!(
            outcome.as_ref().err().map(|fault| fault.category.as_str()),
            Some("PackageLimit"),
            "{release}: {outcome:?}"
        );
        assert_eq!(snapshot(&packages), before, "{release}");
    };
    let unchanged = |prepared: &PreparedPackage| {
        let before = snapshot(&packages);
        let again = publish_import(&data.0, prepared, &idle()).unwrap();
        assert!(
            again.unchanged && !again.committed && again.cleanup_error.is_none(),
            "{again:?}"
        );
        assert_eq!(snapshot(&packages), before);
    };
    // New imports admissible below capacity: another release of the full package from
    // its signer and from another signer, and a new package.
    let next = attempt(&local, "sample", "1.1.0");
    let rival = attempt(&foreign, "sample", "1.2.0");
    let other = attempt(&foreign, "other", "1.0.0");

    // One package's 256 distinct releases from one signer. The first is an ordinary
    // import, so the import lock and folders exist before any comparison.
    let first = attempt(&local, "sample", "1.0.0");
    assert!(publish_import(&data.0, &first, &idle()).unwrap().committed);
    let later: Vec<_> = (1..256)
        .map(|patch| place(&local, "sample", &format!("1.0.{patch}")))
        .collect();
    let catalog = listed();
    assert!(catalog.iter().all(|release| release.package_id == "sample"));
    refuses(&next);
    refuses(&other);
    unchanged(&first);

    // The same capacity spread over packages and signers: the upper half of those
    // releases gives way to another signer's releases of the package and to single
    // releases of other packages from both signers, one short of capacity.
    for relative in &later[127..] {
        fs::remove_file(data.0.join(relative)).unwrap();
    }
    for patch in 0..63 {
        place(&foreign, "sample", &format!("2.0.{patch}"));
    }
    for index in 0..64 {
        let signer = if index % 2 == 0 { &local } else { &foreign };
        place(signer, &format!("spread-{index}"), "1.0.0");
    }
    // Below capacity a new release is admitted, and the filled catalog stays listable.
    let published = publish_import(&data.0, &next, &idle()).unwrap();
    assert!(published.committed && published.cleanup_error.is_none());
    let catalog = listed();
    let package_ids: std::collections::BTreeSet<_> = catalog
        .iter()
        .map(|release| release.package_id.as_str())
        .collect();
    let key_ids: std::collections::BTreeSet<_> = catalog
        .iter()
        .filter_map(|release| release.facts.as_ref())
        .map(|facts| facts.key_id.as_str())
        .collect();
    assert_eq!((package_ids.len(), key_ids.len()), (65, 2));
    unchanged(&next);
    refuses(&other);
    refuses(&rival);
}
