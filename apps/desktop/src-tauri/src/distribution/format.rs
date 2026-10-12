//! Version-1 signed envelope: `index.json` payload facts and the Ed25519
//! `signature.json`. Verification always uses the original index bytes behind the
//! fixed domain prefix, never a JSON reserialization.

use super::{MAX_PAYLOAD_FILES, MAX_SIGNATURE_BYTES, format_fault, limit_fault, signature_fault};
use crate::author_signing::{MAX_INDEX_BYTES, SIGNING_DOMAIN};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use mado_runtime_comparison::images::{PACKAGE_BYTES, PayloadBytes};
use mado_runtime_comparison::inventory::{PackageDraft, os_metadata_name};
use mado_runtime_comparison::model::Fault;
use serde::Serialize;
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const FORMAT: u64 = 1;
const ALGORITHM: &str = "Ed25519";

/// One signed payload fact, in index order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileFact {
    pub(super) path: String,
    pub(super) bytes: usize,
    pub(super) sha256: String,
}

/// The facts a valid signature over the exact envelope bytes covers.
#[derive(Debug)]
pub(super) struct Envelope {
    pub(super) files: Vec<FileFact>,
    pub(super) key_id: String,
}

#[derive(Serialize)]
struct IndexDocument<'a> {
    format: u64,
    files: Vec<IndexFile<'a>>,
}

#[derive(Serialize)]
struct IndexFile<'a> {
    path: &'a str,
    bytes: usize,
    sha256: String,
}

#[derive(Serialize)]
struct SignatureDocument<'a> {
    format: u64,
    algorithm: &'a str,
    public_key: String,
    key_id: &'a str,
    signature: String,
}

/// Lowercase SHA-256 hex, the only digest spelling in this format.
pub(super) fn hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    digest
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(super) fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Index facts come from the captured bytes and their cached digests.
pub(super) fn encode_index(files: &BTreeMap<String, PayloadBytes>) -> Result<Vec<u8>, Fault> {
    if files.is_empty() || files.len() > MAX_PAYLOAD_FILES {
        return Err(limit_fault("a signed package holds 1 to 128 payload files"));
    }
    let mut entries = Vec::with_capacity(files.len());
    for (path, bytes) in files {
        payload_path(path)?;
        entries.push(IndexFile {
            path,
            bytes: bytes.len(),
            sha256: hex(bytes.digest()),
        });
    }
    let index = serde_json::to_vec(&IndexDocument {
        format: FORMAT,
        files: entries,
    })
    .map_err(|error| {
        format_fault("package index could not be encoded")
            .with_context(json!({"cause":error.to_string()}))
    })?;
    if index.len() > MAX_INDEX_BYTES {
        return Err(limit_fault("package index exceeds 1 MiB"));
    }
    Ok(index)
}

pub(super) fn encode_signature(
    public_key: &[u8; 32],
    key_id: &str,
    signature: &[u8; 64],
) -> Result<Vec<u8>, Fault> {
    let bytes = serde_json::to_vec(&SignatureDocument {
        format: FORMAT,
        algorithm: ALGORITHM,
        public_key: STANDARD.encode(public_key),
        key_id,
        signature: STANDARD.encode(signature),
    })
    .map_err(|error| {
        format_fault("package signature could not be encoded")
            .with_context(json!({"cause":error.to_string()}))
    })?;
    if bytes.len() > MAX_SIGNATURE_BYTES {
        return Err(limit_fault("package signature envelope exceeds 16 KiB"));
    }
    Ok(bytes)
}

/// Verifies the signature over `madomata-pkg-v1\n` plus the exact index bytes, then
/// strictly decodes the index. Validity is relative to the included public key only.
pub(super) fn verify(index: &[u8], signature: &[u8]) -> Result<Envelope, Fault> {
    if index.len() > MAX_INDEX_BYTES {
        return Err(limit_fault("package index exceeds 1 MiB"));
    }
    if signature.len() > MAX_SIGNATURE_BYTES {
        return Err(limit_fault("package signature envelope exceeds 16 KiB"));
    }
    let document = strict_json(signature).map_err(|cause| {
        signature_fault("signature.json is not strict JSON").with_context(json!({"cause":cause}))
    })?;
    let fields = members(
        &document,
        &["algorithm", "format", "key_id", "public_key", "signature"],
    )
    .map_err(signature_fault)?;
    if fields["format"].as_u64() != Some(FORMAT) {
        return Err(signature_fault("unsupported signature envelope format"));
    }
    if fields["algorithm"].as_str() != Some(ALGORITHM) {
        return Err(signature_fault("unsupported signature algorithm"));
    }
    let public_key: [u8; 32] = decode_base64(&fields["public_key"])
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| signature_fault("public key must be padded Base64 of 32 bytes"))?;
    let signature_bytes: [u8; 64] = decode_base64(&fields["signature"])
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| signature_fault("signature must be padded Base64 of 64 bytes"))?;
    let key_id = sha256_hex(&public_key);
    if fields["key_id"].as_str() != Some(key_id.as_str()) {
        return Err(signature_fault(
            "key fingerprint disagrees with the included public key",
        ));
    }
    let key = VerifyingKey::from_bytes(&public_key)
        .map_err(|_| signature_fault("included public key is not a valid Ed25519 key"))?;
    let mut message = Vec::with_capacity(SIGNING_DOMAIN.len() + index.len());
    message.extend_from_slice(SIGNING_DOMAIN);
    message.extend_from_slice(index);
    key.verify_strict(&message, &Signature::from_bytes(&signature_bytes))
        .map_err(|_| signature_fault("signature does not verify over the exact package index"))?;
    drop(message);
    let files = decode_index(index)?;
    Ok(Envelope { files, key_id })
}

fn decode_base64(value: &Value) -> Option<Vec<u8>> {
    // STANDARD requires canonical padding and rejects trailing bits.
    STANDARD.decode(value.as_str()?).ok()
}

fn decode_index(index: &[u8]) -> Result<Vec<FileFact>, Fault> {
    let document = strict_json(index).map_err(|cause| {
        format_fault("index.json is not strict JSON").with_context(json!({"cause":cause}))
    })?;
    let fields = members(&document, &["files", "format"]).map_err(format_fault)?;
    if fields["format"].as_u64() != Some(FORMAT) {
        return Err(format_fault("unsupported package index format"));
    }
    let items = fields["files"]
        .as_array()
        .ok_or_else(|| format_fault("index files must be an array"))?;
    if items.is_empty() || items.len() > MAX_PAYLOAD_FILES {
        return Err(limit_fault("a signed package holds 1 to 128 payload files"));
    }
    let mut files: Vec<FileFact> = Vec::with_capacity(items.len());
    let mut aliases = BTreeSet::new();
    for item in items {
        let fields = members(item, &["bytes", "path", "sha256"]).map_err(format_fault)?;
        let path = fields["path"]
            .as_str()
            .ok_or_else(|| format_fault("index path must be a string"))?;
        payload_path(path)?;
        if files
            .last()
            .is_some_and(|previous| previous.path.as_str() >= path)
        {
            return Err(
                format_fault("index files must be unique and sorted by path bytes")
                    .with_context(json!({"path":path})),
            );
        }
        if !aliases.insert(path.to_ascii_lowercase()) {
            return Err(
                format_fault("index paths alias by case").with_context(json!({"path":path}))
            );
        }
        let bytes = fields["bytes"]
            .as_u64()
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or_else(|| format_fault("index byte length must be a non-negative integer"))?;
        if bytes > PACKAGE_BYTES {
            return Err(
                limit_fault("indexed payload file exceeds the package byte bound")
                    .with_context(json!({"path":path})),
            );
        }
        let sha256 = fields["sha256"]
            .as_str()
            .filter(|digest| is_digest(digest))
            .ok_or_else(|| {
                format_fault("index sha256 must be 64 lowercase hex digits")
                    .with_context(json!({"path":path}))
            })?;
        files.push(FileFact {
            path: path.to_owned(),
            bytes,
            sha256: sha256.to_owned(),
        });
    }
    Ok(files)
}

/// Payload-relative names use the existing portable path and OS-metadata rules.
pub(super) fn payload_path(path: &str) -> Result<(), Fault> {
    PackageDraft::check_path(path).map_err(|cause| {
        format_fault("payload path is not portable")
            .with_context(json!({"path":path,"cause":cause}))
    })?;
    if path.rsplit('/').next().is_some_and(os_metadata_name) {
        return Err(format_fault("payload path is a reserved OS metadata name")
            .with_context(json!({"path":path})));
    }
    Ok(())
}

/// An object holding exactly `expected` members (sorted), each present once.
pub(super) fn members<'a>(
    value: &'a Value,
    expected: &[&str],
) -> Result<&'a Map<String, Value>, &'static str> {
    let object = value.as_object().ok_or("JSON value must be an object")?;
    if object.len() != expected.len() || !expected.iter().all(|name| object.contains_key(*name)) {
        return Err("JSON object has missing or unknown members");
    }
    Ok(object)
}

/// Strict UTF-8 JSON; duplicate object members anywhere refuse instead of keeping one.
pub(super) fn strict_json(bytes: &[u8]) -> Result<Value, String> {
    std::str::from_utf8(bytes).map_err(|_| "JSON is not UTF-8".to_owned())?;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let Strict(value) =
        Strict::deserialize(&mut deserializer).map_err(|error| error.to_string())?;
    deserializer.end().map_err(|error| error.to_string())?;
    Ok(value)
}

struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor).map(Strict)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object members")
    }
    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }
    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = sequence.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON object member `{key}`"
                )));
            }
            let Strict(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}
