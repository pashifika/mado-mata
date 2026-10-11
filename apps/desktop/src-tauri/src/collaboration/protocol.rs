//! Closed authoring request envelopes; no arbitrary host-command dispatch.

use super::{FRAME_BYTES, PAGE_UNITS, PROTOCOL};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const REQUEST_ID_BYTES: usize = 128;
const OPAQUE_ID_BYTES: usize = 256;
const ERROR_CODE_BYTES: usize = 64;
const ERROR_MESSAGE_CHARS: usize = 1024;
const DIAGNOSTIC_CHARS: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Outcome {
    NotApplied,
    Unknown,
}

/// A validated request as the main-window controller receives it.
#[derive(Debug, Serialize)]
pub(super) struct Request {
    id: String,
    owner: Option<String>,
    package: Option<String>,
    operation: Operation,
}

impl Request {
    pub(super) fn matches_owner(&self, current: Option<&str>) -> bool {
        matches!(
            self.operation,
            Operation::Describe {}
                | Operation::Notices { .. }
                | Operation::SdkSearch { .. }
                | Operation::SdkDetail { .. }
        ) || self.owner.as_deref() == current
    }
}

#[derive(Debug, Serialize)]
pub struct Claimed {
    ticket: String,
    request: Request,
}

impl Claimed {
    pub(super) fn new(ticket: String, request: Request) -> Self {
        Self { ticket, request }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Describe {},
    Read {
        resource: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    Edit {
        edits: Vec<Edit>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dependencies: Option<Vec<Dependency>>,
    },
    Notices {
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    Save {
        resource: String,
        version: String,
    },
    SaveAll {
        #[serde(skip_serializing_if = "Option::is_none")]
        resources: Option<Vec<Dependency>>,
    },
    CatalogAdd {
        revision: String,
        path: String,
        #[serde(rename = "fileKind")]
        file_kind: String,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        module: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    CatalogRename {
        revision: String,
        path: String,
        destination: String,
    },
    CatalogRemove {
        revision: String,
        path: String,
    },
    Refresh {},
    Validate {
        revision: String,
    },
    Cancel {},
    Create {
        workspace: String,
        #[serde(rename = "packageId")]
        package_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        resolution: Option<Resolution>,
    },
    Open {
        workspace: String,
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        resolution: Option<Resolution>,
    },
    Duplicate {
        #[serde(rename = "packageId")]
        package_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        resolution: Option<Resolution>,
    },
    Exit {
        #[serde(skip_serializing_if = "Option::is_none")]
        resolution: Option<Resolution>,
    },
    SdkSearch {
        query: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    SdkDetail {
        name: String,
    },
    Snippet {
        capture: String,
        mode: String,
        ids: Vec<String>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Resolution {
    Save,
    Discard,
    Cancel,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum Edit {
    Text(TextEdit),
    Ranges(RangesEdit),
    Fields(FieldsEdit),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TextEdit {
    resource: String,
    version: String,
    text: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RangesEdit {
    resource: String,
    version: String,
    ranges: Vec<RangeEdit>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FieldsEdit {
    resource: String,
    version: String,
    fields: Vec<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RangeEdit {
    from: u32,
    to: u32,
    text: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Dependency {
    resource: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestBody {
    owner: Option<String>,
    package: Option<String>,
    operation: Operation,
}

/// A refusal answered on the open connection with the request's id.
#[derive(Debug)]
pub(super) struct Refusal {
    pub code: &'static str,
    pub message: String,
}

fn invalid(message: impl Into<String>) -> Refusal {
    Refusal {
        code: "invalid_request",
        message: message.into(),
    }
}

pub(super) fn valid_request_id(id: &str) -> bool {
    (1..=REQUEST_ID_BYTES).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn valid_opaque(value: &str) -> bool {
    (1..=OPAQUE_ID_BYTES).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn opaque(field: &str, value: &str) -> Result<(), Refusal> {
    if valid_opaque(value) {
        Ok(())
    } else {
        Err(invalid(format!(
            "{field} must be 1 to {OPAQUE_ID_BYTES} printable ASCII characters without spaces"
        )))
    }
}

fn diagnostic(error: &serde_json::Error) -> String {
    let text = error.to_string();
    match text.char_indices().nth(DIAGNOSTIC_CHARS) {
        Some((end, _)) => text[..end].to_owned(),
        None => text,
    }
}

/// Parses the fields left after `protocol`, `instance`, `token` and `id`.
pub(super) fn parse_request(id: String, fields: Map<String, Value>) -> Result<Request, Refusal> {
    let body: RequestBody = serde_json::from_value(Value::Object(fields))
        .map_err(|error| invalid(diagnostic(&error)))?;
    for (field, value) in [("owner", &body.owner), ("package", &body.package)] {
        if let Some(value) = value {
            opaque(field, value)?;
        }
    }
    if body.owner.is_some() != body.package.is_some() {
        return Err(invalid(
            "owner and package must both be present or both null",
        ));
    }
    if !matches!(
        body.operation,
        Operation::Describe {}
            | Operation::Notices { .. }
            | Operation::SdkSearch { .. }
            | Operation::SdkDetail { .. }
            | Operation::Create { .. }
            | Operation::Open { .. }
    ) {
        owned(&body)?;
    }
    check_operation(&body.operation)?;
    Ok(Request {
        id,
        owner: body.owner,
        package: body.package,
        operation: body.operation,
    })
}

fn bounded(field: &str, value: &str, limit: usize, empty: bool) -> Result<(), Refusal> {
    if (!empty && value.is_empty()) || value.contains('\0') || value.encode_utf16().count() > limit
    {
        Err(invalid(format!("{field} exceeds its text allowance")))
    } else {
        Ok(())
    }
}

fn check_operation(operation: &Operation) -> Result<(), Refusal> {
    match operation {
        Operation::Describe {}
        | Operation::Refresh {}
        | Operation::Cancel {}
        | Operation::Exit { .. } => {}
        Operation::Read {
            resource,
            version,
            limit,
            ..
        } => {
            opaque("resource", resource)?;
            if let Some(version) = version {
                opaque("version", version)?;
            }
            if limit.is_some_and(|limit| limit == 0 || limit > PAGE_UNITS) {
                return Err(invalid(format!("limit must be 1 to {PAGE_UNITS}")));
            }
        }
        Operation::Edit {
            edits,
            dependencies,
        } => {
            if edits.is_empty()
                || edits.len() > 64
                || dependencies.as_ref().is_some_and(|items| items.len() > 64)
            {
                return Err(invalid("edit target or dependency allowance exceeded"));
            }
            let mut ranges = 0;
            let mut fields = 0;
            for edit in edits {
                let (resource, version) = match edit {
                    Edit::Text(edit) => (&edit.resource, &edit.version),
                    Edit::Ranges(edit) => {
                        if edit.ranges.is_empty() {
                            return Err(invalid("ranges must not be empty"));
                        }
                        ranges += edit.ranges.len();
                        (&edit.resource, &edit.version)
                    }
                    Edit::Fields(edit) => {
                        if edit.fields.is_empty()
                            || edit.fields.iter().any(|field| !field.is_object())
                        {
                            return Err(invalid("fields must be non-empty structured operations"));
                        }
                        fields += edit.fields.len();
                        (&edit.resource, &edit.version)
                    }
                };
                opaque("resource", resource)?;
                opaque("version", version)?;
            }
            if ranges > 4096 || fields > 256 {
                return Err(invalid("range or field allowance exceeded"));
            }
            for dependency in dependencies.iter().flatten() {
                opaque("resource", &dependency.resource)?;
                opaque("version", &dependency.version)?;
            }
        }
        Operation::Notices { cursor, limit } => {
            if let Some(cursor) = cursor {
                bounded("cursor", cursor, 4096, false)?;
            }
            if limit.is_some_and(|limit| limit == 0 || limit > 256) {
                return Err(invalid("notice limit must be 1 to 256"));
            }
        }
        Operation::Save { resource, version } => {
            opaque("resource", resource)?;
            opaque("version", version)?;
        }
        Operation::SaveAll { resources } => {
            if let Some(resources) = resources {
                if resources.is_empty() || resources.len() > 256 {
                    return Err(invalid("save target allowance exceeded"));
                }
                for resource in resources {
                    opaque("resource", &resource.resource)?;
                    opaque("version", &resource.version)?;
                }
            }
        }
        Operation::CatalogAdd {
            revision,
            path,
            file_kind,
            id,
            module,
            format,
            ..
        } => {
            opaque("revision", revision)?;
            bounded("path", path, 4096, false)?;
            if !matches!(
                file_kind.as_str(),
                "source" | "profile" | "asset" | "source_map"
            ) {
                return Err(invalid("unsupported catalog file kind"));
            }
            for (field, value) in [("id", id), ("module", module), ("format", format)] {
                if let Some(value) = value {
                    bounded(field, value, 4096, false)?;
                }
            }
        }
        Operation::CatalogRename {
            revision,
            path,
            destination,
        } => {
            opaque("revision", revision)?;
            bounded("path", path, 4096, false)?;
            bounded("destination", destination, 4096, false)?;
        }
        Operation::CatalogRemove { revision, path } => {
            opaque("revision", revision)?;
            bounded("path", path, 4096, false)?;
        }
        Operation::Validate { revision } => opaque("revision", revision)?,
        Operation::Create {
            workspace,
            package_id,
            ..
        } => {
            opaque("workspace", workspace)?;
            bounded("packageId", package_id, 256, false)?;
        }
        Operation::Open {
            workspace, path, ..
        } => {
            opaque("workspace", workspace)?;
            bounded("path", path, 4096, false)?;
        }
        Operation::Duplicate { package_id, .. } => bounded("packageId", package_id, 256, false)?,
        Operation::SdkSearch {
            query,
            cursor,
            limit,
        } => {
            bounded("query", query, 256, true)?;
            if let Some(cursor) = cursor {
                bounded("cursor", cursor, 4096, false)?;
            }
            if limit.is_some_and(|limit| limit == 0 || limit > 32) {
                return Err(invalid("SDK limit must be 1 to 32"));
            }
        }
        Operation::SdkDetail { name } => bounded("name", name, 128, false)?,
        Operation::Snippet { capture, mode, ids } => {
            bounded("capture", capture, 256, false)?;
            if !matches!(
                mode.as_str(),
                "game_content" | "ocr_recognize" | "template_recognize"
            ) || ids.len() > 256
            {
                return Err(invalid("snippet kind or definition allowance is invalid"));
            }
            for id in ids {
                bounded("definition id", id, 256, false)?;
            }
        }
    }
    Ok(())
}

fn owned(body: &RequestBody) -> Result<(), Refusal> {
    if body.owner.is_some() && body.package.is_some() {
        Ok(())
    } else {
        Err(Refusal {
            code: "owner_required",
            message: "This operation requires the owner and package returned by describe".into(),
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ErrorBody {
    code: String,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<Outcome>,
}

/// The main-window controller's answer to one claimed request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Response {
    owner: Option<String>,
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ErrorBody>,
}

fn malformed(message: &str) -> Fault {
    Fault::new("CollaborationReply", message)
}

pub(super) fn parse_response(value: Value) -> Result<Response, Fault> {
    let response: Response = serde_json::from_value(value)
        .map_err(|_| malformed("Reply does not match the collaboration protocol"))?;
    check_response(&response)?;
    Ok(response)
}

fn check_response(response: &Response) -> Result<(), Fault> {
    if response
        .owner
        .as_deref()
        .is_some_and(|owner| !valid_opaque(owner))
    {
        return Err(malformed("Reply owner is not a valid opaque identifier"));
    }
    let error = match (response.ok, &response.result, &response.error) {
        (true, Some(_), None) => return Ok(()),
        (false, None, Some(error)) => error,
        _ => {
            return Err(malformed(
                "A successful reply needs only result and a failed reply needs only error",
            ));
        }
    };
    let code = error.code.as_bytes();
    if !(1..=ERROR_CODE_BYTES).contains(&code.len())
        || !code
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        return Err(malformed("Reply error code is not a snake_case identifier"));
    }
    if error.message.chars().count() > ERROR_MESSAGE_CHARS {
        return Err(malformed("Reply error message exceeds its bound"));
    }
    if error
        .resource
        .as_deref()
        .is_some_and(|resource| !valid_opaque(resource))
    {
        return Err(malformed(
            "Reply error resource is not a valid opaque identifier",
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct ErrorRef<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<Outcome>,
}

#[derive(Serialize)]
struct Reply<'a> {
    protocol: u32,
    id: Option<&'a str>,
    instance: &'a str,
    owner: Option<&'a str>,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorRef<'a>>,
}

/// Encodes one frame, or `None` when it would exceed the frame bound.
fn frame(reply: &Reply<'_>) -> Option<Vec<u8>> {
    let mut bytes = vec![0_u8; 4];
    serde_json::to_writer(&mut bytes, reply).ok()?;
    let length = bytes.len() - 4;
    if length > FRAME_BYTES {
        return None;
    }
    let prefix = u32::try_from(length).ok()?.to_be_bytes();
    bytes[..4].copy_from_slice(&prefix);
    Some(bytes)
}

/// Relays the controller's reply, or `None` when it is too large to frame.
pub(super) fn reply_frame(instance: &str, id: &str, response: &Response) -> Option<Vec<u8>> {
    frame(&Reply {
        protocol: PROTOCOL,
        id: Some(id),
        instance,
        owner: response.owner.as_deref(),
        ok: response.ok,
        result: response.result.as_ref(),
        error: response.error.as_ref().map(|error| ErrorRef {
            code: &error.code,
            message: &error.message,
            resource: error.resource.as_deref(),
            outcome: error.outcome,
        }),
    })
}

/// A host-originated refusal. The host does not know the current owner.
pub(super) fn refusal_frame(
    instance: &str,
    id: Option<&str>,
    code: &str,
    message: &str,
    outcome: Outcome,
) -> Vec<u8> {
    frame(&Reply {
        protocol: PROTOCOL,
        id,
        instance,
        owner: None,
        ok: false,
        result: None,
        error: Some(ErrorRef {
            code,
            message,
            resource: None,
            outcome: Some(outcome),
        }),
    })
    // Bounded identifiers and fixed messages always fit in one frame.
    .unwrap_or_default()
}
