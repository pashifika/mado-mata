use super::*;
use mado_runtime_comparison::images::{self, DecodedImage};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "mado-capture-cache-{}",
            crate::storage::new_id().unwrap()
        ));
        Self(root)
    }
    fn cache(&self) -> CaptureCache {
        CaptureCache::new(&self.0).unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn png(value: u8) -> images::EncodedImage {
    let image = DecodedImage::from_rgba(2, 2, vec![value; 16]).unwrap();
    images::encode_crop(&image, [0, 0, 2, 2]).unwrap()
}
fn image_path(directory: &Directory, id: &str) -> PathBuf {
    directory.0.join("caches/package").join(format!("{id}.png"))
}
/// Bytes readable through the verified handle the cache handed out.
fn read_opened(file: Option<fs::File>) -> Vec<u8> {
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file.expect("cache original is available"), &mut bytes)
        .unwrap();
    bytes
}

#[test]
fn selected_configuration_roots_isolate_the_same_capture_key() {
    let first = Directory::new();
    let second = Directory::new();
    let first_cache = first.cache();
    let second_cache = second.cache();
    let id = crate::storage::new_id().unwrap();
    let original = png(17);
    assert!(
        first_cache
            .persist("package", &id, original.as_bytes())
            .cached
    );
    let first_path = image_path(&first, &id);
    let second_path = image_path(&second, &id);
    assert_eq!(
        read_opened(first_cache.open_image("package", &id).unwrap()),
        original.as_bytes()
    );
    assert!(second_cache.open_image("package", &id).unwrap().is_none());
    assert_eq!(fs::read(&first_path).unwrap(), original.as_bytes());
    assert_eq!(
        fs::read(&second_path).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(second_cache.info().bytes, Some(0));
    assert!(!second.0.exists());

    let replacement = png(42);
    assert!(
        second_cache
            .persist("package", &id, replacement.as_bytes())
            .cached
    );
    assert_eq!(fs::read(&first_path).unwrap(), original.as_bytes());
    assert_eq!(fs::read(&second_path).unwrap(), replacement.as_bytes());
    assert_eq!(
        first_cache.folder_for_open().unwrap(),
        first.0.join("caches")
    );
    assert_eq!(
        second_cache.folder_for_open().unwrap(),
        second.0.join("caches")
    );
    assert_eq!(
        first_cache.info().bytes,
        Some(original.as_bytes().len() as u64)
    );
    assert_eq!(
        second_cache.info().bytes,
        Some(replacement.as_bytes().len() as u64)
    );
}

#[test]
fn relative_configuration_root_is_resolved_without_creating_cache_on_measurement() {
    let relative = PathBuf::from(".").join(format!(
        "mado-capture-cache-{}",
        crate::storage::new_id().unwrap()
    ));
    let directory = Directory(std::path::absolute(&relative).unwrap());
    let cache = CaptureCache::new(&relative).unwrap();
    assert_eq!(cache.info().bytes, Some(0));
    assert!(!directory.0.exists());
    let id = crate::storage::new_id().unwrap();
    let original = png(17);
    assert!(cache.persist("package", &id, original.as_bytes()).cached);
    let path = image_path(&directory, &id);
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
    assert_eq!(
        read_opened(cache.open_image("package", &id).unwrap()),
        original.as_bytes()
    );
    assert_eq!(cache.folder_for_open().unwrap(), directory.0.join("caches"));
}

#[test]
fn cache_pngs_and_pending_writes_do_not_enter_or_change_configuration_snapshots() {
    let directory = Directory::new();
    ensure_directories(&directory.0.join("tabs/Closed/package")).unwrap();
    let files = std::collections::BTreeMap::from([
        ("settings.json".to_owned(), b"preserved settings".to_vec()),
        (
            "tabs/Closed/tab.config".to_owned(),
            br#"{"open":false}"#.to_vec(),
        ),
        (
            "tabs/Closed/package/profile.config".to_owned(),
            b"preserved profile".to_vec(),
        ),
    ]);
    for (path, bytes) in &files {
        crate::configuration::write_private(&directory.0.join(path), bytes).unwrap();
    }
    let before = crate::configuration::capture(&directory.0).unwrap();
    assert_eq!(before.files, files);
    let cache = directory.cache();
    let id = crate::storage::new_id().unwrap();
    let original = png(17);
    assert!(cache.persist("package", &id, original.as_bytes()).cached);
    let path = image_path(&directory, &id);
    let pending = path.with_extension("pending");
    fs::write(&pending, b"unresolved cache write").unwrap();
    let captured = crate::configuration::capture(&directory.0).unwrap();
    assert_eq!(captured, before);
    let receipt = crate::backup::write(&directory.0, captured, None).unwrap();
    assert_eq!(
        crate::backup::read(Path::new(&receipt.path)).unwrap(),
        before
    );
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
    assert_eq!(fs::read(&pending).unwrap(), b"unresolved cache write");
    fs::remove_file(pending).unwrap();
    let replacement = png(42);
    assert!(cache.persist("package", &id, replacement.as_bytes()).cached);
    assert_eq!(crate::configuration::capture(&directory.0).unwrap(), before);
    assert_eq!(fs::read(&path).unwrap(), replacement.as_bytes());
    fs::remove_file(path).unwrap();
    assert_eq!(crate::configuration::capture(&directory.0).unwrap(), before);
    for (path, bytes) in files {
        assert_eq!(fs::read(directory.0.join(path)).unwrap(), bytes);
    }
}

#[test]
fn refresh_replaces_only_its_original_and_size_is_remeasured_on_request() {
    let directory = Directory::new();
    let cache = directory.cache();
    assert_eq!(cache.info().bytes, Some(0));
    assert!(!directory.0.exists(), "measurement must not create storage");
    let id = crate::storage::new_id().unwrap();
    let original = png(17);
    let published = cache.persist("package", &id, original.as_bytes());
    assert!(published.cached && published.error.is_none());
    let path = image_path(&directory, &id);
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
    let other_id = crate::storage::new_id().unwrap();
    assert!(
        cache
            .persist("package", &other_id, original.as_bytes())
            .cached
    );
    let next = png(42);
    let replacement = cache.persist("package", &id, next.as_bytes());
    assert!(replacement.cached && replacement.error.is_none());
    assert_eq!(fs::read(&path).unwrap(), next.as_bytes());
    assert_eq!(
        read_opened(cache.open_image("package", &other_id).unwrap()),
        original.as_bytes()
    );
    assert!(!path.with_extension("pending").exists());
    assert_eq!(
        cache.info().bytes,
        Some((original.as_bytes().len() + next.as_bytes().len()) as u64)
    );
    fs::remove_file(path).unwrap();
    assert_eq!(cache.info().bytes, Some(original.as_bytes().len() as u64));
}

#[test]
fn failed_refresh_keeps_complete_previous_cache_and_can_be_retried() {
    let directory = Directory::new();
    let cache = directory.cache();
    let id = crate::storage::new_id().unwrap();
    let original = png(17);
    assert!(cache.persist("package", &id, original.as_bytes()).cached);
    let path = image_path(&directory, &id);
    let pending = path.with_extension("pending");
    fs::write(&pending, b"another writer").unwrap();
    let replacement = png(42);
    let refused = cache.persist("package", &id, replacement.as_bytes());
    assert!(!refused.cached && refused.error.is_some());
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
    assert_eq!(fs::read(&pending).unwrap(), b"another writer");
    fs::remove_file(&pending).unwrap();
    let retried = cache.persist("package", &id, replacement.as_bytes());
    assert!(retried.cached && retried.error.is_none());
    assert_eq!(fs::read(&path).unwrap(), replacement.as_bytes());
}

#[test]
fn failed_write_preserves_foreign_pending_content_and_redacts_local_paths() {
    let directory = Directory::new();
    let cache = directory.cache();
    let id = crate::storage::new_id().unwrap();
    let path = image_path(&directory, &id);
    ensure_directories(path.parent().unwrap()).unwrap();
    let pending = path.with_extension("pending");
    fs::write(&pending, b"unresolved write").unwrap();
    let result = cache.persist("package", &id, png(17).as_bytes());
    assert!(!result.cached);
    assert_eq!(fs::read(pending).unwrap(), b"unresolved write");
    assert!(!path.exists());
    let diagnostic = serde_json::to_string(&result.error.unwrap()).unwrap();
    assert!(!diagnostic.contains(directory.0.to_str().unwrap()));
    assert!(cache.open_image("../outside", &id).is_err());
    assert!(cache.open_image("package", "../outside").is_err());
    assert!(!cache.persist("../outside", &id, png(17).as_bytes()).cached);
    assert!(!directory.0.join("caches/../outside").exists());
}

#[cfg(unix)]
#[test]
fn substituted_folders_and_link_entries_refuse_measurement_open_and_reload() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let outside = Directory::new();
    fs::create_dir_all(&directory.0).unwrap();
    fs::create_dir_all(&outside.0).unwrap();
    fs::write(outside.0.join("private.png"), b"outside").unwrap();
    let cache = directory.cache();
    let id = crate::storage::new_id().unwrap();
    let original = png(17);
    let outside_entries = || {
        let mut names: Vec<_> = fs::read_dir(&outside.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let untouched = outside_entries();
    symlink(&outside.0, &cache.folder).unwrap();
    assert!(cache.info().bytes.is_none());
    assert!(cache.folder_for_open().is_err());
    assert!(cache.open_image("package", &id).is_err());
    let write = cache.persist("package", &id, original.as_bytes());
    assert!(!write.cached && write.error.is_some());
    assert_eq!(
        outside_entries(),
        untouched,
        "no write reaches a linked cache root"
    );
    fs::remove_file(&cache.folder).unwrap();
    ensure_directories(&cache.folder).unwrap();
    symlink(
        outside.0.join("private.png"),
        cache.folder.join("linked.png"),
    )
    .unwrap();
    let info = cache.info();
    assert!(
        info.bytes.is_none() && info.error.is_some(),
        "a refused entry must not produce a complete partial total"
    );
    fs::remove_file(cache.folder.join("linked.png")).unwrap();

    // A linked package folder is refused for both reading and writing.
    fs::write(outside.0.join(format!("{id}.png")), original.as_bytes()).unwrap();
    let untouched = outside_entries();
    symlink(&outside.0, cache.folder.join("package")).unwrap();
    assert!(cache.open_image("package", &id).is_err());
    let write = cache.persist("package", &id, png(42).as_bytes());
    assert!(!write.cached && write.error.is_some());
    assert_eq!(outside_entries(), untouched);
    assert_eq!(
        fs::read(outside.0.join(format!("{id}.png"))).unwrap(),
        original.as_bytes()
    );
    fs::remove_file(cache.folder.join("package")).unwrap();

    // A linked original inside a genuine package folder is refused, never followed.
    let path = image_path(&directory, &id);
    ensure_directories(path.parent().unwrap()).unwrap();
    symlink(outside.0.join(format!("{id}.png")), &path).unwrap();
    assert!(cache.open_image("package", &id).is_err());
    assert!(cache.info().bytes.is_none());
    // Publication replaces only the link entry; the outside original stays intact.
    assert!(cache.persist("package", &id, png(42).as_bytes()).cached);
    assert_eq!(fs::read(&path).unwrap(), png(42).as_bytes());
    assert!(!fs::symlink_metadata(&path).unwrap().is_symlink());
    assert_eq!(
        fs::read(outside.0.join(format!("{id}.png"))).unwrap(),
        original.as_bytes()
    );
    assert_eq!(
        read_opened(cache.open_image("package", &id).unwrap()),
        png(42).as_bytes()
    );
}
