//! The finite adapter payload embedded by the build and its app-owned copies.
//!
//! A prepared copy lives at `<data root>/plugins/omp/<version>-<content>/` and is
//! published only after its bytes verify, with an atomic no-replace rename. An
//! existing copy is verified and reused, never overwritten or removed.

use super::version::Version;
use crate::configuration::{
    create_private_directory, publish_no_replace, sync_directory, temporary, write_private,
};
use crate::storage::{self, private_directory};
use mado_runtime_comparison::inventory::is_os_metadata_entry;
use mado_runtime_comparison::model::Fault;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// The adapter identity OMP registers.
pub(super) const NAME: &str = "@madomata/omp-authoring";
/// The actual adapter release before application-supplied distribution.
pub(super) const PREDECESSOR: &str = "0.1.0";
/// SHA-256 of the actual predecessor `index.mjs` and `client.mjs`.
const PREDECESSOR_RUNTIME: [&str; 2] = [
    "922c85039d4454b198e19ceb1783c58b82a63834c7a67d5c83afaef2a71eda80",
    "d9a48b15efa1d6be5ce1c617970d9227ea5e2dbb1992354f62eae242337ac4e6",
];
const ENTRY: &str = "index.mjs";
const OMP_PACKAGE: &str = "@oh-my-pi/pi-coding-agent";
pub(super) const PLUGINS: &str = "plugins";
const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_PAYLOAD_ENTRIES: usize = 16;

/// Exactly the files OMP loads, in identity order. Tests, dependencies and
/// private configuration are not part of the distribution.
pub(super) const FILES: [(&str, &[u8]); 3] = [
    (
        "package.json",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../integrations/omp/package.json"
        )),
    ),
    (
        ENTRY,
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../integrations/omp/index.mjs"
        )),
    ),
    (
        "client.mjs",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../integrations/omp/client.mjs"
        )),
    ),
];

#[derive(Deserialize)]
struct Manifest {
    name: String,
    version: String,
    #[serde(default)]
    omp: Option<OmpManifest>,
    #[serde(default, rename = "peerDependencies")]
    peers: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct OmpManifest {
    #[serde(default)]
    extensions: Vec<String>,
}

impl Manifest {
    fn parse(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }

    fn compatible(&self) -> bool {
        self.name == NAME
            && self
                .omp
                .as_ref()
                .is_some_and(|omp| omp.extensions == [ENTRY])
    }
}

/// The release this application build supplies.
pub(super) struct Included {
    pub(super) version: Version,
    pub(super) content: String,
    pub(super) minimum: Version,
}

pub(super) fn included() -> Result<&'static Included, Fault> {
    static INCLUDED: LazyLock<Result<Included, Fault>> = LazyLock::new(|| {
        let invalid = |message: &str| Fault::new("PluginPayload", message);
        let manifest = Manifest::parse(FILES[0].1)
            .ok_or_else(|| invalid("the included adapter manifest is malformed"))?;
        if !manifest.compatible() {
            return Err(invalid(
                "the included adapter manifest does not name the known adapter entry",
            ));
        }
        let version = Version::parse(&manifest.version)
            .ok_or_else(|| invalid("the included adapter version is malformed"))?;
        let minimum = manifest
            .peers
            .get(OMP_PACKAGE)
            .and_then(|range| range.strip_prefix(">="))
            .and_then(Version::parse)
            .filter(|minimum| !minimum.is_prerelease())
            .ok_or_else(|| invalid("the included adapter has no minimum OMP release"))?;
        Ok(Included {
            version,
            content: identity(FILES),
            minimum,
        })
    });
    INCLUDED.as_ref().map_err(Clone::clone)
}

/// Content identity over file names and bytes in [`FILES`] order.
pub(super) fn identity<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"madomata-omp-adapter-v1\0");
    for (name, bytes) in files {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    format!("{:x}", hasher.finalize())
}

pub(super) fn base(root: &Path) -> PathBuf {
    root.join(PLUGINS).join("omp")
}

fn directory_name(version: &str, content: &str) -> String {
    format!("{version}-{content}")
}

fn integrity(message: &str) -> Fault {
    Fault::new("PluginPayload", message)
}

fn io_fault(operation: &str, error: &io::Error) -> Fault {
    Fault::new("PluginPayload", format!("could not {operation}")).with_context(
        json!({"operation": operation, "kind": format!("{:?}", error.kind()), "os_code": error.raw_os_error()}),
    )
}

/// Returns the canonical directory holding a verified copy of the included release.
pub(super) fn prepare(root: &Path, included: &Included) -> Result<PathBuf, Fault> {
    private_directory(&root.join(PLUGINS))?;
    let base = base(root);
    private_directory(&base)?;
    let destination = base.join(directory_name(
        &included.version.to_string(),
        &included.content,
    ));
    if !storage::exists(&destination)? {
        let staging = temporary(&base, "omp-payload");
        create_private_directory(&staging)?;
        // A staged copy is never registered: only its verified, published name is.
        let published = stage(&staging)
            .and_then(|()| verified(contents(&staging), included))
            .and_then(|()| {
                publish_no_replace(&staging, &destination)
                    .map_err(|error| io_fault("publish the adapter payload", &error))
            });
        match published {
            Ok(()) => sync_directory(&base)?,
            Err(fault) => {
                // Only this call's staging directory is removed, never a published copy.
                let _ = fs::remove_dir_all(&staging);
                if !storage::exists(&destination)? {
                    return Err(fault);
                }
            }
        }
    }
    let canonical = destination
        .canonicalize()
        .map_err(|error| io_fault("resolve the prepared adapter payload", &error))?;
    verified(managed(&canonical), included).map_err(|fault| {
        integrity(
            "an existing prepared adapter payload differs from its identity; it was preserved",
        )
        .with_context(json!({"path": canonical.to_string_lossy(), "cause": fault}))
    })?;
    Ok(canonical)
}

fn stage(staging: &Path) -> Result<(), Fault> {
    for (name, bytes) in FILES {
        write_private(&staging.join(name), bytes)?;
    }
    sync_directory(staging)
}

fn verified(payload: Result<Payload, Fault>, included: &Included) -> Result<(), Fault> {
    let payload = payload?;
    if payload.version != included.version || payload.content != included.content {
        return Err(integrity(
            "the prepared adapter payload is not the included release",
        ));
    }
    Ok(())
}

/// A strictly verified app-owned copy.
pub(super) struct Payload {
    pub(super) version: Version,
    pub(super) content: String,
}

/// Verifies a published app-owned copy: its contents and its identity-named location.
pub(super) fn managed(directory: &Path) -> Result<Payload, Fault> {
    let payload = contents(directory)?;
    if directory.file_name().and_then(|name| name.to_str())
        != Some(directory_name(&payload.version.to_string(), &payload.content).as_str())
    {
        return Err(integrity(
            "the adapter payload location does not match its identity",
        ));
    }
    Ok(payload)
}

/// Verifies a private directory holding exactly the payload files.
fn contents(directory: &Path) -> Result<Payload, Fault> {
    storage::check_directory(directory)?;
    let mut names = BTreeSet::new();
    for (index, entry) in fs::read_dir(directory)
        .map_err(|error| io_fault("list the adapter payload", &error))?
        .enumerate()
    {
        if index >= MAX_PAYLOAD_ENTRIES {
            return Err(integrity("the adapter payload has unexpected entries"));
        }
        let entry = entry.map_err(|error| io_fault("read the adapter payload", &error))?;
        let name = entry.file_name();
        if is_os_metadata_entry(&name, &entry)
            .map_err(|error| io_fault("inspect the adapter payload", &error))?
        {
            continue;
        }
        names.insert(name);
    }
    if names.len() != FILES.len()
        || FILES
            .iter()
            .any(|(name, _)| !names.contains(std::ffi::OsStr::new(name)))
    {
        return Err(integrity("the adapter payload has unexpected entries"));
    }
    let mut files = Vec::with_capacity(FILES.len());
    for (name, _) in FILES {
        files.push(storage::read_bytes(&directory.join(name), MAX_FILE_BYTES)?);
    }
    let manifest = Manifest::parse(&files[0])
        .filter(Manifest::compatible)
        .ok_or_else(|| integrity("the adapter payload manifest is not the known adapter"))?;
    let version = Version::parse(&manifest.version)
        .ok_or_else(|| integrity("the adapter payload version is malformed"))?;
    let content = identity(
        FILES
            .iter()
            .zip(&files)
            .map(|((name, _), bytes)| (*name, bytes.as_slice())),
    );
    Ok(Payload { version, content })
}

/// Facts read from a registration source this application does not own.
pub(super) struct Source {
    pub(super) name: Option<String>,
    pub(super) version: Option<String>,
    pub(super) content: Option<String>,
    /// The actual predecessor or included runtime with a compatible manifest.
    pub(super) recognized: bool,
}

pub(super) fn source(directory: &Path, included: &Included) -> Source {
    let files = FILES.map(|(name, _)| foreign_file(&directory.join(name)));
    let manifest = files[0].as_deref().and_then(Manifest::parse);
    let content = files.iter().all(Option::is_some).then(|| {
        identity(
            FILES
                .iter()
                .zip(&files)
                .map(|((name, _), bytes)| (*name, bytes.as_deref().unwrap_or_default())),
        )
    });
    let runtime = [files[1].as_deref(), files[2].as_deref()];
    let recognized = manifest.as_ref().is_some_and(|manifest| {
        manifest.compatible()
            && if manifest.version == PREDECESSOR {
                runtime
                    .into_iter()
                    .zip(PREDECESSOR_RUNTIME)
                    .all(|(bytes, digest)| {
                        bytes.is_some_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == digest)
                    })
            } else {
                Version::parse(&manifest.version).is_some_and(|version| version == included.version)
                    && runtime == [Some(FILES[1].1), Some(FILES[2].1)]
            }
    });
    Source {
        name: manifest.as_ref().map(|manifest| manifest.name.clone()),
        version: manifest.map(|manifest| manifest.version),
        content,
        recognized,
    }
}

/// A bounded regular file; links and oversized files are not read.
fn foreign_file(path: &Path) -> Option<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_FILE_BYTES).then_some(bytes)
}
