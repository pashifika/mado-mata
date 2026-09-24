use crate::inventory::Inventory;
use crate::model::{Fault, MAX_TRANSPORT_BYTES, Plan, encode_bounded};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Operation {
    Run,
    EnvironmentCheck,
}

impl Operation {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::EnvironmentCheck => "environment_check",
        }
    }

    pub(super) fn validate(self, plan: &Plan) -> Result<(), Fault> {
        if self == Self::EnvironmentCheck {
            let configuration = plan.native_config.as_ref().ok_or_else(|| {
                crate::environment::blocked(
                    "configuration_unset",
                    "environment check requires a replay configuration",
                )
            })?;
            let configuration: crate::environment::Configuration<Value> =
                serde_json::from_value(configuration.clone()).map_err(|error| {
                    crate::environment::blocked("configuration_validation", &error.to_string())
                })?;
            if plan.lane != "replay"
                || configuration.version != 1
                || configuration.native.is_some()
                || configuration.replay.is_none()
            {
                return Err(crate::environment::blocked(
                    "configuration_validation",
                    "environment check accepts only non-native replay initialization",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Invocation {
    pub(super) run: String,
    pub(super) attempt: u64,
    pub(super) operation: Operation,
    pub(super) plan: Plan,
    pub(super) inventory: Inventory,
    #[serde(default)]
    pub(super) observe_logs: bool,
}

pub fn read_json<T: DeserializeOwned>(path: &Path, bound: usize) -> Result<T, Fault> {
    let file = File::open(path).map_err(|e| Fault::new("Read", e.to_string()))?;
    let mut bytes = Vec::new();
    file.take(bound as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Fault::new("Read", e.to_string()))?;
    if bytes.len() > bound {
        return Err(Fault::new(
            "LimitExceeded",
            "JSON input exceeds its byte bound",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|e| Fault::new("InvalidJson", e.to_string()))
}

pub(super) fn frame(reader: &mut impl BufRead, bound: usize) -> Result<Option<Vec<u8>>, Fault> {
    let mut bytes = Vec::new();
    reader
        .take(bound as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|e| Fault::new("Transport", e.to_string()))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > bound {
        return Err(Fault::new("Transport", "oversized protocol frame")
            .with_context(json!({"frame_error":"Oversized","bytes_received":bytes.len()})));
    }
    if bytes.last() != Some(&b'\n') {
        return Err(Fault::new("Transport", "incomplete protocol frame")
            .with_context(json!({"frame_error":"Incomplete","bytes_received":bytes.len()})));
    }
    Ok(Some(bytes))
}

pub(super) fn emit(value: &Value) -> Result<(), Fault> {
    let bytes = encode_bounded(value, MAX_TRANSPORT_BYTES - 1)?;
    emit_bytes(&bytes)
}

pub(super) fn emit_bytes(bytes: &[u8]) -> Result<(), Fault> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(bytes)
        .and_then(|()| stdout.write_all(b"\n"))
        .and_then(|()| stdout.flush())
        .map_err(|e| Fault::new("Transport", e.to_string()))
}
