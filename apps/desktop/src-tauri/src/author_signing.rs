//! Local author signing. One validated Google enrollment publishes one OS-random Ed25519
//! key per data root in `keys/active.json`; later signing is offline and limited to the
//! domain-prefixed package index. The plaintext record is user-managed, never generated
//! implicitly and never enters configuration capture, packages, frontend responses or logs.
mod oidc;
#[cfg(test)]
mod tests;

use crate::configuration::{
    create_private_file, digest, publish_no_replace, sync_directory, temporary,
};
use crate::storage::{check_directory, exists, read_bytes};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer as _, SigningKey};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Exact UTF-8 domain prefix of every package signature message.
pub const SIGNING_DOMAIN: &[u8] = b"madomata-pkg-v1\n";
pub const MAX_INDEX_BYTES: usize = 1024 * 1024;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const RECORD_VERSION: u32 = 1;
const ALGORITHM: &str = "Ed25519";
const KEYS: &str = "keys";
const ACTIVE: &str = "active.json";
const PROVIDER: &str = "google";
const MAX_IDENTIFIER_BYTES: usize = 255;
const MAX_LABEL_BYTES: usize = 256;

/// Public facts about the local signer. The cached account association is local only and
/// never proof of a current login or a verified publisher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SigningInfo {
    pub key_id: String,
    pub display_label: String,
    pub enrolled_at: u64,
    pub provider: &'static str,
    pub local_only: bool,
}

/// The usable active key of one data root. It signs only bounded package indexes.
pub struct Signer {
    key: SigningKey,
    key_id: String,
}

impl Signer {
    /// Opens and validates the existing record; a missing or unusable key is an error,
    /// never a reason to generate a replacement.
    pub fn open(root: &Path) -> Result<Self, Fault> {
        load(root)?.map(|(signer, _)| signer).ok_or_else(|| {
            Fault::new(
                "SigningKeyMissing",
                "No local signing key is enrolled for this data root; enrollment creates a new identity",
            )
        })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// Lowercase SHA-256 hex digest of the 32-byte public key.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Signs `SIGNING_DOMAIN` followed by the original index bytes.
    pub fn sign_index(&self, index: &[u8]) -> Result<[u8; 64], Fault> {
        if index.len() > MAX_INDEX_BYTES {
            return Err(Fault::new(
                "SigningLimit",
                "The package index exceeds 1 MiB and was not signed",
            ));
        }
        let mut message = Vec::with_capacity(SIGNING_DOMAIN.len() + index.len());
        message.extend_from_slice(SIGNING_DOMAIN);
        message.extend_from_slice(index);
        Ok(self.key.sign(&message).to_bytes())
    }

    fn from_key(key: SigningKey) -> Self {
        let key_id = digest(key.verifying_key().as_bytes());
        Self { key, key_id }
    }

    /// Independent format fixtures sign without key files or enrollment.
    #[cfg(test)]
    pub(crate) fn from_seed(seed: [u8; 32]) -> Self {
        Self::from_key(SigningKey::from_bytes(&seed))
    }
}

impl fmt::Debug for Signer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Signer")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// `None` only when no key store or active record exists; an unusable record is an error.
pub fn status(root: &Path) -> Result<Option<SigningInfo>, Fault> {
    Ok(load(root)?.map(|(_, info)| info))
}

/// Runs one Google login and publishes a new key only after complete token validation.
/// The caller opens `open_browser` with the generated authorization URL and cancels
/// through `cancelled`; listener, verifier and tokens never outlive this call.
pub fn enroll(
    root: &Path,
    cancelled: &AtomicBool,
    open_browser: impl FnOnce(&str) -> Result<(), Fault>,
) -> Result<SigningInfo, Fault> {
    check_directory(root)?;
    if load(root)?.is_some() {
        return Err(occupied());
    }
    let account = oidc::login(&oidc::GOOGLE, cancelled, open_browser)?;
    publish(root, &account, cancelled, |_| Ok(()))
}

/// Publishes an OS-random key with a fixed local association through the real publication
/// path, for host tests that need a usable signer without enrollment.
#[cfg(test)]
pub(crate) fn test_signer(root: &Path) -> Signer {
    let account = Account {
        issuer: oidc::GOOGLE.issuer.to_owned(),
        subject: "fixture-subject".to_owned(),
        client_id: oidc::GOOGLE.client_id.to_owned(),
        display_label: "Fixture author".to_owned(),
    };
    publish(root, &account, &AtomicBool::new(false), |_| Ok(()))
        .expect("publish the fixture signing key");
    Signer::open(root).expect("reopen the fixture signing key")
}

/// Validated local association facts; private to the key record.
struct Account {
    issuer: String,
    subject: String,
    client_id: String,
    display_label: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    algorithm: String,
    seed: String,
    public_key: String,
    key_id: String,
    issuer: String,
    subject: String,
    client_id: String,
    display_label: String,
    enrolled_at: u64,
}

impl Record {
    fn new(signer: &Signer, account: &Account, enrolled_at: u64) -> Self {
        Self {
            version: RECORD_VERSION,
            algorithm: ALGORITHM.to_owned(),
            seed: STANDARD.encode(signer.key.to_bytes()),
            public_key: STANDARD.encode(signer.public_key()),
            key_id: signer.key_id.clone(),
            issuer: account.issuer.clone(),
            subject: account.subject.clone(),
            client_id: account.client_id.clone(),
            display_label: account.display_label.clone(),
            enrolled_at,
        }
    }

    fn open(self) -> Result<(Signer, SigningInfo), Fault> {
        if self.version != RECORD_VERSION || self.algorithm != ALGORITHM {
            return Err(unusable(
                "The signing key record has an unsupported version or algorithm",
            ));
        }
        let signer = Signer::from_key(SigningKey::from_bytes(&key_bytes(&self.seed)?));
        if signer.public_key() != key_bytes(&self.public_key)? || signer.key_id != self.key_id {
            return Err(unusable(
                "The signing key record's seed, public key and fingerprint are inconsistent",
            ));
        }
        if self.issuer != oidc::GOOGLE.issuer
            || !bounded_text(&self.subject, MAX_IDENTIFIER_BYTES)
            || !bounded_text(&self.client_id, MAX_IDENTIFIER_BYTES)
            || !bounded_text(&self.display_label, MAX_LABEL_BYTES)
            || self.enrolled_at > 8_640_000_000_000
        {
            return Err(unusable(
                "The signing key record's local account association is unsupported",
            ));
        }
        let info = SigningInfo {
            key_id: self.key_id,
            display_label: self.display_label,
            enrolled_at: self.enrolled_at,
            provider: PROVIDER,
            local_only: true,
        };
        Ok((signer, info))
    }
}

fn load(root: &Path) -> Result<Option<(Signer, SigningInfo)>, Fault> {
    let keys = root.join(KEYS);
    if !exists(&keys)? {
        return Ok(None);
    }
    check_directory(&keys).map_err(|fault| at(fault, KEYS))?;
    let path = keys.join(ACTIVE);
    if !exists(&path)? {
        return Ok(None);
    }
    let bytes =
        read_bytes(&path, MAX_RECORD_BYTES).map_err(|fault| at(fault, "keys/active.json"))?;
    // Parser messages can quote field values; never let seed text reach a fault.
    if bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        != Some(b'{')
    {
        return Err(unusable("The signing key record must be a JSON object"));
    }
    let record: Record = serde_json::from_slice(&bytes)
        .map_err(|_| unusable("The signing key record is malformed or has unsupported fields"))?;
    record.open().map(Some)
}

/// Generates and publishes a key without replacing any record. Cancellation is honored
/// up to the no-replace rename; after that the durable key is the outcome.
fn publish(
    root: &Path,
    account: &Account,
    cancelled: &AtomicBool,
    before_publish: impl FnOnce(&Path) -> Result<(), Fault>,
) -> Result<SigningInfo, Fault> {
    check_cancelled(cancelled)?;
    check_directory(root)?;
    let keys = root.join(KEYS);
    key_directory(&keys)?;
    let destination = keys.join(ACTIVE);
    if exists(&destination)? {
        return Err(occupied());
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|_| {
        Fault::new(
            "SigningRandom",
            "The operating system random source is unavailable; no key was created",
        )
    })?;
    let signer = Signer::from_key(SigningKey::from_bytes(&seed));
    let bytes = crate::storage::encode(
        &Record::new(&signer, account, now_seconds()?),
        MAX_RECORD_BYTES,
    )?;
    let staging = temporary(&keys, "active");
    // Cleanup below may remove only the staging file this call created.
    let file = create_private_file(&staging)?;
    let staged = stage(file, &bytes).and_then(|()| {
        before_publish(&staging)?;
        check_cancelled(cancelled)?;
        publish_no_replace(&staging, &destination).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                occupied()
            } else {
                storage("publish the signing key without replacement", &error)
            }
        })
    });
    if let Err(mut fault) = staged {
        match fs::remove_file(&staging) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                fault.context["cleanup_error"] =
                    json!(storage("remove the signing key staging file", &error));
            }
        }
        return Err(fault);
    }
    // Published: a later failure is reported with the durable key, never as a rollback.
    sync_directory(&keys)
        .and_then(|()| load(root))
        .and_then(|loaded| match loaded {
            Some((reopened, info)) if reopened.public_key() == signer.public_key() => Ok(info),
            _ => Err(unusable(
                "The published signing key could not be reopened consistently",
            )),
        })
        .map_err(|mut fault| {
            fault.context["published"] = json!(true);
            fault
        })
}

fn stage(mut file: fs::File, bytes: &[u8]) -> Result<(), Fault> {
    file.write_all(bytes)
        .map_err(|error| storage("write the signing key", &error))?;
    #[cfg(unix)]
    restrict(&file, 0o600)?;
    file.sync_all()
        .map_err(|error| storage("sync the signing key", &error))
}

/// Creates or adopts only `keys/` itself; the configuration root is never changed.
/// Windows keeps ordinary inherited permissions.
fn key_directory(keys: &Path) -> Result<(), Fault> {
    #[cfg_attr(
        not(unix),
        expect(unused_mut, reason = "only Unix sets the key-store mode")
    )]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(keys) {
        Ok(()) => {}
        // A competing enrollment may have created it; adopt only a real directory below.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(storage("create the key store", &error)),
    }
    let named =
        fs::symlink_metadata(keys).map_err(|error| storage("inspect the key store", &error))?;
    if !named.is_dir() {
        return Err(Fault::new(
            "SigningStorage",
            "The key store must be a real directory, not a file or link",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let directory =
            fs::File::open(keys).map_err(|error| storage("open the key store", &error))?;
        let opened = directory
            .metadata()
            .map_err(|error| storage("inspect the key store", &error))?;
        if !opened.is_dir() || opened.dev() != named.dev() || opened.ino() != named.ino() {
            return Err(Fault::new(
                "SigningStorage",
                "The key store changed while being opened",
            ));
        }
        restrict(&directory, 0o700)?;
    }
    Ok(())
}

/// Sets and then checks the exact mode through the opened handle.
#[cfg(unix)]
fn restrict(file: &fs::File, mode: u32) -> Result<(), Fault> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|error| storage("set key-store permissions", &error))?;
    let actual = file
        .metadata()
        .map_err(|error| storage("inspect key-store permissions", &error))?
        .mode()
        & 0o7777;
    if actual == mode {
        Ok(())
    } else {
        Err(Fault::new(
            "SigningStorage",
            "Key-store permissions could not be established",
        )
        .with_context(json!({"expected": format!("{mode:o}"), "actual": format!("{actual:o}")})))
    }
}

fn key_bytes(text: &str) -> Result<[u8; 32], Fault> {
    let invalid = || unusable("The signing key record contains an invalid key encoding");
    if text.len() != 44 {
        return Err(invalid());
    }
    let decoded = STANDARD.decode(text).map_err(|_| invalid())?;
    <[u8; 32]>::try_from(decoded.as_slice()).map_err(|_| invalid())
}

fn bounded_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn now_seconds() -> Result<u64, Fault> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_| Fault::new("SigningClock", "The system clock precedes the Unix epoch"))
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), Fault> {
    if cancelled.load(Ordering::Acquire) {
        Err(cancelled_fault())
    } else {
        Ok(())
    }
}

fn cancelled_fault() -> Fault {
    Fault::new(
        "SigningCancelled",
        "Signing enrollment was cancelled; no key was published",
    )
}

fn occupied() -> Fault {
    Fault::new(
        "SigningKeyExists",
        "This data root already has an active signing key; it was not replaced",
    )
}

fn unusable(message: &str) -> Fault {
    Fault::new("SigningKey", message).with_context(json!({"path": "keys/active.json"}))
}

fn at(mut fault: Fault, path: &str) -> Fault {
    fault.context["path"] = json!(path);
    fault
}

fn storage(operation: &str, error: &io::Error) -> Fault {
    Fault::new("SigningStorage", format!("could not {operation}")).with_context(json!({
        "operation": operation,
        "kind": format!("{:?}", error.kind()),
        "os_code": error.raw_os_error(),
    }))
}
