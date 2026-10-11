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
    prepared.archive.as_ref().unwrap().to_vec()
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
    assert!(imported.archive.is_none());
    assert_eq!(imported.draft().files(), prepared.draft().files());
    imported.draft().validate().unwrap();
    // Confirmation uses the captured artifact, never a reopened replacement.
    fs::write(&destination, b"replacement after review").unwrap();

    let installed = publish_import(&data.0, &imported, &idle()).unwrap();
    assert!(installed.committed && !installed.unchanged && installed.cleanup_error.is_none());
    let container = data
        .0
        .canonicalize()
        .unwrap()
        .join("pkgs/sample")
        .join(&facts.artifact_id);
    assert_eq!(Path::new(&installed.path), container);
    assert_eq!(
        fs::read(container.join("index.json")).unwrap(),
        member(&entries, "index.json")
    );
    assert_eq!(
        fs::read(container.join("signature.json")).unwrap(),
        member(&entries, "signature.json")
    );
    assert_eq!(
        fs::read(container.join("package/main.ts")).unwrap(),
        SOURCE.as_bytes()
    );
    let receipt: Value =
        serde_json::from_slice(&fs::read(container.join("receipt.json")).unwrap()).unwrap();
    assert_eq!(
        receipt,
        json!({
            "version":1,"artifact_id":facts.artifact_id,"package_id":"sample","release_version":"1.0.0",
            "key_id":facts.key_id,"index_sha256":format::sha256_hex(&member(&entries, "index.json"))
        })
    );

    let verified = verify_installed(&data.0, "sample", &facts.artifact_id).unwrap();
    assert_eq!(verified.facts(), &facts);
    assert_eq!(verified.payload_path(), container.join("package"));
    assert_eq!(verified.draft().files(), prepared.draft().files());
    verified.draft().validate().unwrap();

    let again = publish_import(&data.0, &imported, &idle()).unwrap();
    assert!(again.unchanged && !again.committed && again.cleanup_error.is_none());
    assert_eq!(
        names(container.parent().unwrap()),
        [facts.artifact_id.clone()]
    );

    let listed = installed_releases(&data.0).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].facts.as_ref(), Some(&facts));
    assert!(listed[0].fault.is_none());

    let identity = InstalledIdentity {
        package_id: "sample".into(),
        artifact_id: facts.artifact_id.clone(),
    };
    for path in [
        container.clone(),
        container.join("package"),
        container.join("package/profiles/default.json"),
        container.join("package/missing/new.ts"),
    ] {
        assert_eq!(
            managed_identity(&data.0, &path).unwrap(),
            Some(identity.clone()),
            "{path:?}"
        );
    }
    assert_eq!(
        managed_identity(&data.0, &data.0.join("pkgs/sample")).unwrap(),
        None
    );
    assert_eq!(managed_identity(&data.0, &output.0).unwrap(), None);
    assert_eq!(
        managed_identity(&output.0.join("absent"), &container).unwrap(),
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
    assert!(installed_releases(&data.0).unwrap().is_empty());
    for path in [&plain, &digest_named] {
        assert_eq!(managed_identity(&data.0, path).unwrap(), None);
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
    let package = data.0.join("pkgs/sample");
    assert_eq!(names(&package), [original.facts().artifact_id.clone()]);

    // Distinct release versions coexist; nothing chooses a latest one.
    let next = import(&selection, &archive_of(&export("1.1.0"))).unwrap();
    publish_import(&data.0, &next, &idle()).unwrap();
    let listed = installed_releases(&data.0).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|release| release.fault.is_none()));
    verify_installed(&data.0, "sample", &original.facts().artifact_id).unwrap();
    verify_installed(&data.0, "sample", &next.facts().artifact_id).unwrap();
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

    // A compressible non-image file cannot use unused image or envelope allowance.
    let mut oversized = files("1.0.0");
    oversized.insert(
        "main.ts".into(),
        payload(vec![b' '; PACKAGE_NON_IMAGE_BYTES + 1]),
    );
    let large_index: Value =
        serde_json::from_slice(&format::encode_index(&oversized).unwrap()).unwrap();
    let large = replaced(
        &entries,
        "package/main.ts",
        vec![b' '; PACKAGE_NON_IMAGE_BYTES + 1],
    );
    let fault = import(
        &selection,
        &zip_of(&resigned(&large, &large_index), CompressionMethod::Deflated),
    )
    .unwrap_err();
    assert!(fault.message.contains("non-image"), "{fault:?}");

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
    let imported = import(&selection, &archive_of(&export("1.0.0"))).unwrap();
    let artifact = imported.facts().artifact_id.clone();
    let package = data.0.join("pkgs/sample");

    // An ordinary editable package at the release location is preserved, not adopted.
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("package.json"), b"{}").unwrap();
    assert_eq!(
        publish_import(&data.0, &imported, &idle())
            .unwrap_err()
            .category,
        "PackageConflict"
    );
    assert_eq!(names(&package), ["package.json"]);
    fs::remove_dir_all(&package).unwrap();
    fs::write(data.0.join("pkgs/package.json"), b"{}").unwrap();
    assert_eq!(
        publish_import(&data.0, &imported, &idle())
            .unwrap_err()
            .category,
        "PackageConflict"
    );
    assert!(!package.exists());
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
    let fault = installed::publish(&data.0, &imported, &cancel, &mut |stage, _| {
        if stage == Stage::Staged {
            cancel.store(true, Ordering::Release);
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(fault.category, "PackageCancelled");
    assert!(names(&package).is_empty());
    assert!(installed_releases(&data.0).unwrap().is_empty());

    read_only(&package);
    let fault = publish_import(&data.0, &imported, &idle()).unwrap_err();
    set_mode(&package, 0o700);
    assert_eq!(fault.category, "ConfigurationIo", "{fault:?}");
    assert!(names(&package).is_empty());

    let fault = installed::publish(&data.0, &imported, &idle(), &mut |stage, path| {
        if stage == Stage::Publish {
            fs::create_dir(path).unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(fault.category, "PackageConflict");
    assert_eq!(names(&package), [artifact.clone()]);
    assert!(names(&package.join(&artifact)).is_empty());
    fs::remove_dir(package.join(&artifact)).unwrap();

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
    assert!(installed_releases(&data.0).unwrap().is_empty());
    fs::remove_dir_all(package.join(&retained[0])).unwrap();

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
    assert_eq!(names(&package), [artifact.clone()]);
    assert_eq!(
        verify_installed(&data.0, "sample", &artifact)
            .unwrap()
            .facts(),
        imported.facts()
    );
}

#[test]
fn installed_tampering_refuses_and_reimport_never_repairs() {
    let selection = Root::new();
    let archive = archive_of(&export("1.0.0"));
    let cases: [(&str, fn(&Path), &str); 9] = [
        (
            "payload byte",
            |c: &Path| {
                rewrite(c.join("package/main.ts"), |mut bytes| {
                    bytes[0] ^= 1;
                    bytes
                });
            },
            "InstalledIntegrity",
        ),
        (
            "payload extra",
            |c: &Path| fs::write(c.join("package/extra.ts"), b"export {};").unwrap(),
            "InstalledIntegrity",
        ),
        (
            "payload missing file",
            |c: &Path| fs::remove_file(c.join("package/schema.json")).unwrap(),
            "InstalledIntegrity",
        ),
        (
            "reserialized index",
            |c: &Path| {
                rewrite(c.join("index.json"), |bytes| {
                    serde_json::to_vec_pretty(&serde_json::from_slice::<Value>(&bytes).unwrap())
                        .unwrap()
                });
            },
            "InstalledIntegrity",
        ),
        (
            "receipt release",
            |c: &Path| {
                rewrite(c.join("receipt.json"), |bytes| {
                    let mut receipt: Value = serde_json::from_slice(&bytes).unwrap();
                    receipt["release_version"] = json!("9.9.9");
                    serde_json::to_vec(&receipt).unwrap()
                });
            },
            "InstalledIntegrity",
        ),
        (
            "container extra",
            |c: &Path| fs::write(c.join("notes.txt"), b"unsigned").unwrap(),
            "InstalledIntegrity",
        ),
        (
            "signature missing",
            |c: &Path| fs::remove_file(c.join("signature.json")).unwrap(),
            "InstalledMissing",
        ),
        (
            "receipt missing",
            |c: &Path| fs::remove_file(c.join("receipt.json")).unwrap(),
            "InstalledMissing",
        ),
        (
            "payload missing",
            |c: &Path| fs::remove_dir_all(c.join("package")).unwrap(),
            "InstalledMissing",
        ),
    ];
    for (label, tamper, category) in cases {
        let data = Root::new();
        let imported = import(&selection, &archive).unwrap();
        publish_import(&data.0, &imported, &idle()).unwrap();
        let artifact = imported.facts().artifact_id.clone();
        let container = data.0.join("pkgs/sample").join(&artifact);
        tamper(&container);
        let fault = verify_installed(&data.0, "sample", &artifact).unwrap_err();
        assert_eq!(fault.category, category, "{label}: {fault:?}");
        assert_eq!(fault.context["artifact_id"], artifact.as_str(), "{label}");
        let listed = installed_releases(&data.0).unwrap();
        assert_eq!(listed.len(), 1, "{label}");
        assert!(listed[0].facts.is_none(), "{label}");
        assert_eq!(
            listed[0].fault.as_ref().unwrap().category,
            category,
            "{label}"
        );
        // A managed container stays copy-before-edit even with a broken envelope.
        assert!(
            managed_identity(&data.0, &container.join("package"))
                .unwrap()
                .is_some(),
            "{label}"
        );
        let before = snapshot(&container);
        let fault = publish_import(&data.0, &imported, &idle()).unwrap_err();
        assert_eq!(fault.category, "InstalledIntegrity", "{label}: {fault:?}");
        assert_eq!(snapshot(&container), before, "{label}");
    }

    let data = Root::new();
    let imported = import(&selection, &archive).unwrap();
    publish_import(&data.0, &imported, &idle()).unwrap();
    let artifact = imported.facts().artifact_id.clone();
    fs::remove_dir_all(data.0.join("pkgs/sample").join(&artifact)).unwrap();
    assert_eq!(
        verify_installed(&data.0, "sample", &artifact)
            .unwrap_err()
            .category,
        "InstalledMissing"
    );
    assert!(installed_releases(&data.0).unwrap().is_empty());
}
