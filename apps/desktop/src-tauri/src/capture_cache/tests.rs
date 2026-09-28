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
        CaptureCache::new(self.0.clone()).unwrap()
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
    let path = cache.image_path("package", &id).unwrap();
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
        fs::read(cache.image_path("package", &other_id).unwrap()).unwrap(),
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
    let path = cache.image_path("package", &id).unwrap();
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
    let path = cache.image_path("package", &id).unwrap();
    ensure_directories(path.parent().unwrap()).unwrap();
    let pending = path.with_extension("pending");
    fs::write(&pending, b"unresolved write").unwrap();
    let result = cache.persist("package", &id, png(17).as_bytes());
    assert!(!result.cached);
    assert_eq!(fs::read(pending).unwrap(), b"unresolved write");
    assert!(!path.exists());
    let diagnostic = serde_json::to_string(&result.error.unwrap()).unwrap();
    assert!(!diagnostic.contains(directory.0.to_str().unwrap()));
    assert!(cache.image_path("../outside", &id).is_err());
    assert!(cache.image_path("package", "../outside").is_err());
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
    symlink(&outside.0, &cache.folder).unwrap();
    assert!(cache.info().bytes.is_none());
    assert!(cache.folder_for_open().is_err());
    assert!(
        cache
            .image_path("package", &crate::storage::new_id().unwrap())
            .is_err()
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
}
