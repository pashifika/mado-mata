//! Non-native configuration and immutable resource/corpus capture.

use crate::inventory::Inventory;
use crate::model::{Control, ENGINE_REVISION, Fault, Limits, identity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const G004_PROFILE: &str = "g-004-rapidocr-ppocrv4-det-v6-rec-small-v1";
pub const BOUNDED_PROFILE: &str = "phase-3-1-rapidocr-ppocrv4-det-v6-rec-small-bounded-v2";
pub const LANGUAGE: &str = "horizontal-ja-basic-latin-ascii-digits-ui-symbols-v1";
pub const PROVIDER: &str = "cpu";
pub const RUNTIME_PROFILE: &str = "onnxruntime-1.29.0-api17-cpu";
const MAX_PATH_BYTES: usize = 4096;
const MAX_LIBRARIES: usize = 64;
const MAX_RESOURCE_BYTES: u64 = 1_073_741_824;
const MAX_DESCRIPTOR_BYTES: usize = 262_144;
// Content identities of both accepted profiles at ENGINE_REVISION.
const MODELS: [(&str, u64, &str); 2] = [
    (
        "rapidocr-v3.9.2/ch_PP-OCRv4_det_mobile.onnx",
        4_745_517,
        "d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9",
    ),
    (
        "rapidocr-v3.9.2/PP-OCRv6_rec_small.onnx",
        21_234_383,
        "6f327246b50388f3c176ae304bd95767ea6dc0c9ae92153ef8cbe210b3c14884",
    ),
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OcrEnvironment {
    pub model: String,
    pub profile: String,
    pub language: String,
    pub provider: String,
    pub runtime_profile: String,
    pub model_root: String,
    pub runtime_path: String,
    pub native_library_paths: Vec<String>,
}

impl OcrEnvironment {
    /// Structural validation only: saving never reads files or initializes a backend.
    pub fn validate(&self) -> Result<(), Fault> {
        validate_tuple(
            &self.model,
            &self.profile,
            &self.language,
            &self.provider,
            &self.runtime_profile,
        )?;
        validate_path(&self.model_root)?;
        validate_path(&self.runtime_path)?;
        if self.native_library_paths.is_empty() || self.native_library_paths.len() > MAX_LIBRARIES {
            return Err(blocked(
                "native_libraries_unset",
                "select between 1 and 64 reviewed native library files",
            ));
        }
        let mut seen = BTreeSet::new();
        for path in &self.native_library_paths {
            validate_path(path)?;
            if !seen.insert(path) {
                return Err(blocked(
                    "configuration_validation",
                    "native library locations must be distinct",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct EnvironmentSnapshot {
    pub configuration: Value,
    pub identity: String,
}

#[derive(Clone, Debug)]
pub struct ReplaySnapshot {
    pub configuration: Value,
    pub identity: String,
    pub corpus_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Configuration<N> {
    pub version: u32,
    pub ocr: OcrConfig,
    pub native_libraries: Vec<Library>,
    pub replay: Option<ReplayConfig>,
    pub native: Option<N>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Library {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OcrConfig {
    pub model: String,
    pub profile: String,
    pub language: String,
    pub provider: String,
    pub runtime_profile: String,
    pub model_root: PathBuf,
    pub runtime: Library,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplayConfig {
    pub corpus_id: String,
    pub frames: Vec<RecordedFrame>,
    pub package_entries: BTreeMap<String, String>,
    pub templates: BTreeMap<String, String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordedFrame {
    pub asset: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub captured_ns: u64,
    pub discontinuous: bool,
    pub placement: Option<Placement>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Placement {
    pub desktop_origin: [f64; 2],
    pub logical_size: [f64; 2],
    pub scale: [f64; 2],
}

pub fn capture_environment(
    environment: &OcrEnvironment,
    control: &Control,
) -> Result<EnvironmentSnapshot, Fault> {
    control.check()?;
    environment.validate()?;
    let model_root = PathBuf::from(&environment.model_root)
        .canonicalize()
        .map_err(|error| io_fault("model_root", error))?;
    validate_models(&model_root, control)?;
    let runtime_path = Path::new(&environment.runtime_path)
        .canonicalize()
        .map_err(|error| io_fault("runtime_validation", error))?;
    let runtime = capture_library(&runtime_path, control, "runtime_validation")?;
    let native_libraries = environment
        .native_library_paths
        .iter()
        .map(|path| {
            let path = Path::new(path)
                .canonicalize()
                .map_err(|error| io_fault("native_library_validation", error))?;
            capture_library(&path, control, "native_library_validation")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let configuration = serde_json::to_value(Configuration::<Value> {
        version: 1,
        ocr: OcrConfig {
            model: environment.model.clone(),
            profile: environment.profile.clone(),
            language: environment.language.clone(),
            provider: environment.provider.clone(),
            runtime_profile: environment.runtime_profile.clone(),
            model_root,
            runtime,
        },
        native_libraries,
        native: None,
        replay: None,
    })
    .map_err(|error| Fault::new("Encoding", error.to_string()))?;
    control.check()?;
    Ok(EnvironmentSnapshot {
        identity: identity(&(ENGINE_REVISION, &configuration, MODELS))?,
        configuration,
    })
}

pub fn capture_replay(
    path: &Path,
    inventory: &Inventory,
    limits: &Limits,
    control: &Control,
) -> Result<ReplaySnapshot, Fault> {
    control.check()?;
    limits.validate()?;
    inventory.validate()?;
    let resolved = path
        .canonicalize()
        .map_err(|error| io_fault("replay_descriptor", error))?;
    let bound = MAX_DESCRIPTOR_BYTES.min(limits.snapshot_bytes);
    let mut bytes = Vec::new();
    read_stable(
        &resolved,
        bound as u64,
        control,
        "replay_descriptor",
        |chunk| bytes.extend_from_slice(chunk),
    )?;
    if path
        .canonicalize()
        .map_err(|error| io_fault("replay_descriptor", error))?
        != resolved
    {
        return Err(changed("replay_descriptor"));
    }
    let config: ReplayConfig = serde_json::from_slice(&bytes)
        .map_err(|error| blocked("replay_validation", &error.to_string()))?;
    validate_replay(&config, &inventory.assets, limits, control)?;
    let declarations = &inventory.metadata["manifest"]["assets"];
    for frame in &config.frames {
        control.check()?;
        let asset = &declarations[&frame.asset];
        if asset["format"] != "raw-rgba8"
            || frame.pixel_format != "rgba8"
            || asset["width"].as_u64() != Some(u64::from(frame.width))
            || asset["height"].as_u64() != Some(u64::from(frame.height))
        {
            return Err(blocked(
                "replay_validation",
                "recorded frame geometry/format disagrees with the captured asset declaration",
            ));
        }
    }
    for (entry, asset) in &config.package_entries {
        let format = declarations[asset]["format"].as_str();
        if (entry == "madopilot-package.json" && format != Some("json"))
            || (entry != "madopilot-package.json" && !matches!(format, Some("png" | "json")))
        {
            return Err(blocked(
                "replay_validation",
                "engine package entries require captured JSON manifests or PNG templates",
            ));
        }
    }
    let corpus_id = config.corpus_id.clone();
    let configuration =
        serde_json::to_value(config).map_err(|error| Fault::new("Encoding", error.to_string()))?;
    control.check()?;
    Ok(ReplaySnapshot {
        identity: identity(&(ENGINE_REVISION, &configuration, &inventory.identity))?,
        configuration,
        corpus_id,
    })
}

pub(crate) fn blocked(stage: &str, reason: &str) -> Fault {
    Fault::new("Blocked", reason)
        .with_context(json!({"stage":stage,"engine_revision":ENGINE_REVISION}))
}

fn validate_path(path: &str) -> Result<(), Fault> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.chars().any(char::is_control)
        || !Path::new(path).is_absolute()
    {
        return Err(blocked(
            "configuration_validation",
            "resource paths must be bounded absolute paths without control characters",
        ));
    }
    Ok(())
}

fn validate_tuple(
    model: &str,
    profile: &str,
    language: &str,
    provider: &str,
    runtime_profile: &str,
) -> Result<(), Fault> {
    if !matches!(profile, G004_PROFILE | BOUNDED_PROFILE)
        || model != profile
        || language != LANGUAGE
        || provider != PROVIDER
        || runtime_profile != RUNTIME_PROFILE
    {
        return Err(blocked(
            "ocr_unsupported",
            "model/profile/language/runtime/provider combination is unsupported; no provider fallback is permitted",
        ));
    }
    Ok(())
}

#[cfg(feature = "engine")]
pub(crate) fn validate_ocr<N>(config: &Configuration<N>, control: &Control) -> Result<(), Fault> {
    let ocr = &config.ocr;
    validate_tuple(
        &ocr.model,
        &ocr.profile,
        &ocr.language,
        &ocr.provider,
        &ocr.runtime_profile,
    )?;
    validate_models(&ocr.model_root, control)?;
    verify_file(&ocr.runtime, control, "runtime_validation")?;
    if config.native_libraries.is_empty() {
        return Err(blocked(
            "native_libraries_unset",
            "identify the linked OpenCV and other non-system native library files",
        ));
    }
    for library in &config.native_libraries {
        verify_file(library, control, "native_library_validation")?;
    }
    control.check()
}

fn validate_models(root: &Path, control: &Control) -> Result<(), Fault> {
    control.check()?;
    canonical(root, true, "model_root")?;
    for (relative, bytes, sha256) in MODELS {
        verify_file(
            &Library {
                path: root.join(relative),
                sha256: sha256.into(),
                bytes,
            },
            control,
            "model_validation",
        )?;
    }
    Ok(())
}

pub(crate) fn canonical(path: &Path, directory: bool, stage: &str) -> Result<(), Fault> {
    if !path.is_absolute() {
        return Err(blocked(
            stage,
            "configured path must be absolute and canonical",
        ));
    }
    let resolved = path
        .canonicalize()
        .map_err(|error| io_fault(stage, error))?;
    if resolved != path {
        return Err(blocked(
            stage,
            "configured path must be canonical; symlink aliases are not accepted",
        ));
    }
    let metadata = resolved
        .metadata()
        .map_err(|error| io_fault(stage, error))?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(blocked(
            stage,
            "configured prerequisite has the wrong file type",
        ));
    }
    Ok(())
}

fn io_fault(stage: &str, error: std::io::Error) -> Fault {
    blocked(stage, "configured prerequisite is missing or unreadable")
        .with_context(json!({"stage":stage,"io_kind":format!("{:?}",error.kind()),"engine_revision":ENGINE_REVISION}))
}

fn changed(stage: &str) -> Fault {
    blocked(
        "configuration_changed",
        "configured resource changed during capture or no longer matches its immutable identity",
    )
    .with_context(
        json!({"stage":stage,"reason":"configuration_changed","engine_revision":ENGINE_REVISION}),
    )
}

#[derive(PartialEq, Eq)]
struct Stamp {
    bytes: u64,
    modified: SystemTime,
    identity: [u64; 4],
}

fn stamp(metadata: &Metadata, stage: &str) -> Result<Stamp, Fault> {
    if !metadata.is_file() {
        return Err(blocked(
            stage,
            "configured prerequisite must be a regular file",
        ));
    }
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        [
            metadata.dev(),
            metadata.ino(),
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ]
    };
    #[cfg(windows)]
    let identity = {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(blocked(stage, "reparse points are not accepted"));
        }
        [
            metadata.creation_time(),
            metadata.last_write_time(),
            u64::from(metadata.file_attributes()),
            0,
        ]
    };
    #[cfg(not(any(unix, windows)))]
    return Err(blocked(
        stage,
        "resource identity is unsupported on this platform",
    ));
    #[cfg(any(unix, windows))]
    Ok(Stamp {
        bytes: metadata.len(),
        modified: metadata
            .modified()
            .map_err(|error| io_fault(stage, error))?,
        identity,
    })
}

// The open handle and path must still name the same regular file after bounded,
// cancellable reads. Nonblocking/no-follow opens prevent a raced FIFO from hanging Stop.
fn read_stable(
    path: &Path,
    bound: u64,
    control: &Control,
    stage: &str,
    mut consume: impl FnMut(&[u8]),
) -> Result<u64, Fault> {
    control.check()?;
    canonical(path, false, stage)?;
    let before = stamp(
        &path.metadata().map_err(|error| io_fault(stage, error))?,
        stage,
    )?;
    if before.bytes == 0 || before.bytes > bound {
        return Err(blocked(
            stage,
            "resource is empty or exceeds its byte bound",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x100 | 0x4); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000 | 0x800); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x00200000); // FILE_SHARE_READ | OPEN_REPARSE_POINT
    }
    let mut file: File = options.open(path).map_err(|error| io_fault(stage, error))?;
    if stamp(
        &file.metadata().map_err(|error| io_fault(stage, error))?,
        stage,
    )? != before
    {
        return Err(changed(stage));
    }
    let mut remaining = before.bytes;
    let mut buffer = [0u8; 65_536];
    while remaining > 0 {
        control.check()?;
        let length = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..length])
            .map_err(|error| io_fault(stage, error))?;
        consume(&buffer[..length]);
        remaining -= length as u64;
    }
    control.check()?;
    let mut trailing = [0u8; 1];
    if file
        .read(&mut trailing)
        .map_err(|error| io_fault(stage, error))?
        != 0
        || stamp(
            &file.metadata().map_err(|error| io_fault(stage, error))?,
            stage,
        )? != before
        || stamp(
            &path.metadata().map_err(|error| io_fault(stage, error))?,
            stage,
        )? != before
    {
        return Err(changed(stage));
    }
    canonical(path, false, stage)?;
    Ok(before.bytes)
}

fn capture_library(path: &Path, control: &Control, stage: &str) -> Result<Library, Fault> {
    let mut hash = Sha256::new();
    let bytes = read_stable(path, MAX_RESOURCE_BYTES, control, stage, |chunk| {
        hash.update(chunk)
    })?;
    Ok(Library {
        path: path.to_owned(),
        sha256: format!("{:x}", hash.finalize()),
        bytes,
    })
}

fn verify_file(library: &Library, control: &Control, stage: &str) -> Result<(), Fault> {
    if library.bytes == 0
        || library.bytes > MAX_RESOURCE_BYTES
        || library.sha256.len() != 64
        || !library
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(blocked(
            "configuration_validation",
            "each resource requires a finite size (at most 1 GiB) and lowercase SHA-256",
        ));
    }
    control.check()?;
    canonical(&library.path, false, stage)?;
    if library
        .path
        .metadata()
        .map_err(|error| io_fault(stage, error))?
        .len()
        != library.bytes
    {
        return Err(changed(stage));
    }
    let mut hash = Sha256::new();
    let bytes = read_stable(&library.path, library.bytes, control, stage, |chunk| {
        hash.update(chunk)
    })?;
    if bytes != library.bytes || format!("{:x}", hash.finalize()) != library.sha256 {
        return Err(changed(stage));
    }
    Ok(())
}

fn package_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 240
        && path.split('/').count() <= 32
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.chars().any(|ch| {
                    ch.is_control() || matches!(ch, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                })
        })
}

pub(crate) fn validate_replay(
    config: &ReplayConfig,
    assets: &BTreeMap<String, Vec<u8>>,
    limits: &Limits,
    control: &Control,
) -> Result<(), Fault> {
    control.check()?;
    if config.corpus_id.is_empty()
        || config.corpus_id.len() > 240
        || config.corpus_id.chars().any(char::is_control)
        || config.frames.is_empty()
        || config.frames.len() > limits.snapshot_files
        || config.package_entries.is_empty()
        || config.package_entries.len() > limits.snapshot_files
        || !config
            .package_entries
            .contains_key("madopilot-package.json")
        || config.templates.is_empty()
        || config.templates.len() > limits.handles
    {
        return Err(blocked(
            "replay_validation",
            "recorded corpus, bounded frames, template package entries and template aliases are required",
        ));
    }
    let mut bytes = 0usize;
    let mut timestamp = None;
    for record in &config.frames {
        control.check()?;
        if !matches!(record.pixel_format.as_str(), "rgba8" | "bgra8") {
            return Err(blocked(
                "replay_unsupported",
                "replay supports packed rgba8 or bgra8 frames",
            ));
        }
        if timestamp.is_some_and(|previous| previous >= record.captured_ns) {
            return Err(blocked(
                "replay_validation",
                "recorded timestamps must be strictly increasing",
            ));
        }
        timestamp = Some(record.captured_ns);
        let pixels = assets.get(&record.asset).ok_or_else(|| {
            blocked(
                "replay_asset_missing",
                "recorded frame asset is not in the captured inventory",
            )
        })?;
        let expected = (record.width as usize)
            .checked_mul(record.height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if record.width == 0 || record.height == 0 || expected != Some(pixels.len()) {
            return Err(blocked(
                "replay_descriptor",
                "recorded frame dimensions and packed pixel length must agree",
            ));
        }
        bytes = bytes
            .checked_add(pixels.len())
            .ok_or_else(|| blocked("replay_limit", "replay byte count overflow"))?;
        if bytes > limits.snapshot_bytes {
            return Err(blocked(
                "replay_limit",
                "replay corpus exceeds snapshot_bytes",
            ));
        }
        if let Some(placement) = &record.placement {
            if placement.desktop_origin.iter().any(|v| !v.is_finite())
                || placement
                    .logical_size
                    .iter()
                    .chain(&placement.scale)
                    .any(|v| !v.is_finite() || *v <= 0.0)
            {
                return Err(blocked(
                    "geometry",
                    "recorded placement requires finite origin and positive finite size and scale",
                ));
            }
        }
    }
    let mut entries = BTreeSet::new();
    for (entry, asset) in &config.package_entries {
        control.check()?;
        if !package_path(entry) || !entries.insert(entry.to_ascii_lowercase()) {
            return Err(blocked(
                "template_validation",
                "engine package paths must be bounded, relative, distinct and non-escaping",
            ));
        }
        if !assets.contains_key(asset) {
            return Err(blocked(
                "template_asset_missing",
                "template package references an uncaptured asset",
            ));
        }
    }
    for (alias, template) in &config.templates {
        if !package_path(alias) || !package_path(template) {
            return Err(blocked(
                "template_validation",
                "template aliases and identifiers must be bounded and non-escaping",
            ));
        }
    }
    control.check()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Plan;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::UNIX_EPOCH;

    struct CapturedFile(PathBuf);

    impl CapturedFile {
        fn new(bytes: &[u8]) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "mado-environment-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            let mut file = File::create_new(&path).unwrap();
            file.write_all(bytes).unwrap();
            Self(path)
        }
    }

    impl Drop for CapturedFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn limits() -> Limits {
        serde_json::from_str::<Plan>(include_str!("../fixtures/controlled-plan.json"))
            .unwrap()
            .limits
    }

    #[test]
    fn same_path_same_length_replacement_invalidates_captured_resource() {
        let file = CapturedFile::new(b"first-resource");
        let control = Control::new(&limits());
        let captured = capture_library(&file.0, &control, "runtime_validation").unwrap();
        std::fs::write(&file.0, b"other-resource").unwrap();
        let error = verify_file(&captured, &control, "runtime_validation").unwrap_err();
        assert_eq!(error.context["reason"], "configuration_changed");
        assert_ne!(
            capture_library(&file.0, &control, "runtime_validation")
                .unwrap()
                .sha256,
            captured.sha256
        );
    }

    #[test]
    fn cancellation_interrupts_resource_capture_between_chunks() {
        let file = CapturedFile::new(&vec![7; 131_072]);
        let control = Control::new(&limits());
        let mut observed_bytes = 0;
        let error = read_stable(
            &file.0,
            MAX_RESOURCE_BYTES,
            &control,
            "runtime_validation",
            |chunk| {
                observed_bytes += chunk.len();
                control.cancel();
            },
        )
        .unwrap_err();
        assert_eq!(error.category, "Cancelled");
        assert!(observed_bytes < 131_072);
        assert_eq!(
            capture_library(&file.0, &control, "runtime_validation")
                .err()
                .unwrap()
                .category,
            "Cancelled"
        );
    }

    #[test]
    fn replay_descriptor_binds_inventory_and_refuses_authority_geometry_and_escape() {
        let limits = limits();
        let control = Control::new(&limits);
        let mut inventory = Inventory::capture(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
            &limits,
        )
        .unwrap();
        inventory
            .assets
            .insert("engine-manifest".into(), b"{}".to_vec());
        inventory.metadata["manifest"]["assets"]["engine-manifest"] = json!({
            "path":"assets/engine.json","format":"json","width":0,"height":0,
        });
        inventory.refresh_identity().unwrap();
        let descriptor = json!({
            "corpus_id":"recorded-boundary",
            "frames":[
                {"asset":"marker","width":2,"height":2,"pixel_format":"rgba8","captured_ns":0,"discontinuous":false,"placement":null},
                {"asset":"marker","width":2,"height":2,"pixel_format":"rgba8","captured_ns":1,"discontinuous":true,"placement":null},
            ],
            "package_entries":{"madopilot-package.json":"engine-manifest"},
            "templates":{"marker":"marker"},
        });
        let file = CapturedFile::new(&serde_json::to_vec(&descriptor).unwrap());
        let captured = capture_replay(&file.0, &inventory, &limits, &control).unwrap();
        inventory.assets.get_mut("marker").unwrap()[0] ^= 1;
        inventory.refresh_identity().unwrap();
        assert_ne!(
            captured.identity,
            capture_replay(&file.0, &inventory, &limits, &control)
                .unwrap()
                .identity
        );
        for (scenario, pointer, replacement) in [
            ("native authority", "/native", json!({"approved":true})),
            (
                "unknown frame field",
                "/frames/0/path",
                json!("/private/frame"),
            ),
            (
                "same-byte geometry mismatch",
                "/frames/0",
                json!({
                    "asset":"marker","width":1,"height":4,"pixel_format":"rgba8","captured_ns":0,"discontinuous":false,"placement":null,
                }),
            ),
            ("pixel format mismatch", "/frames/0/pixel_format", json!("bgra8")),
            ("nonincreasing timestamp", "/frames/1/captured_ns", json!(0)),
            (
                "uncaptured frame",
                "/frames/0/asset",
                json!("external-frame"),
            ),
            (
                "escaping package entry",
                "/package_entries",
                json!({"madopilot-package.json":"engine-manifest","../escape.png":"marker"}),
            ),
            (
                "invalid placement",
                "/frames/0/placement",
                json!({"desktop_origin":[0,0],"logical_size":[2,2],"scale":[0,1]}),
            ),
        ] {
            let mut invalid = descriptor.clone();
            // Object additions exercise strict unknown-field rejection as well.
            match pointer {
                "/native" => invalid["native"] = replacement,
                "/frames/0/path" => invalid["frames"][0]["path"] = replacement,
                _ => *invalid.pointer_mut(pointer).unwrap() = replacement,
            }
            std::fs::write(&file.0, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(
                capture_replay(&file.0, &inventory, &limits, &control).is_err(),
                "{scenario}"
            );
        }
    }
}
