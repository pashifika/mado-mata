use super::object;
use crate::model::Fault;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// The supported schema is deliberately small; unsupported keywords never become annotations.
pub fn resolve_options(schema: &Value, profile: &Value, package_id: &str) -> Result<Value, Fault> {
    let root = object(
        schema,
        &[
            "version",
            "type",
            "properties",
            "required",
            "additionalProperties",
            "default",
        ],
        &[
            "version",
            "type",
            "properties",
            "required",
            "additionalProperties",
        ],
    )
    .map_err(|error| Fault::new("Schema", error.message))?;
    if root["version"].as_u64() != Some(1) || root["type"] != "object" {
        return Err(Fault::new(
            "Schema",
            "unsupported root schema version or type",
        ));
    }
    validate_schema(schema, "$", true, 0)?;
    let profile = object(
        profile,
        &["package_id", "schema_version", "options"],
        &["package_id", "schema_version", "options"],
    )
    .map_err(|error| Fault::new("Profile", error.message))?;
    if profile["package_id"].as_str() != Some(package_id)
        || profile["schema_version"] != root["version"]
    {
        return Err(Fault::new(
            "ProfileIdentity",
            "profile package or schema identity does not match",
        ));
    }
    let supplied = profile["options"]
        .as_object()
        .ok_or_else(|| Fault::new("Profile", "options must be an object"))?;
    let properties = root["properties"]
        .as_object()
        .ok_or_else(|| Fault::new("Schema", "properties must be an object"))?;
    for key in supplied.keys() {
        if !properties.contains_key(key) {
            return Err(Fault::new("Profile", format!("unknown option: {key}")));
        }
    }
    let mut resolved = Map::new();
    for (key, node) in properties {
        if let Some(value) = supplied.get(key).or_else(|| node.get("default")) {
            resolved.insert(key.clone(), value.clone());
        }
    }
    let resolved = Value::Object(resolved);
    validate_value(schema, &resolved, "$")?;
    Ok(resolved)
}

fn validate_schema(schema: &Value, path: &str, root: bool, depth: usize) -> Result<(), Fault> {
    if depth > 32 {
        return Err(Fault::new("Schema", "schema nesting exceeds 32"));
    }
    let map = schema
        .as_object()
        .ok_or_else(|| Fault::new("Schema", format!("{path}: schema must be an object")))?;
    let kind = map
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Fault::new("Schema", format!("{path}: type is required")))?;
    let specific: &[&str] = match kind {
        "object" => &["properties", "required", "additionalProperties"],
        "array" => &["items", "minItems", "maxItems"],
        "string" => &["enum", "minLength", "maxLength"],
        "integer" | "number" => &["minimum", "maximum"],
        "boolean" => &[],
        _ => {
            return Err(Fault::new(
                "Schema",
                format!("{path}: unsupported type {kind}"),
            ));
        }
    };
    for key in map.keys() {
        if key != "type"
            && key != "default"
            && !(root && key == "version")
            && !specific.contains(&key.as_str())
        {
            return Err(Fault::new(
                "Schema",
                format!("{path}: unsupported keyword {key}"),
            ));
        }
    }
    match kind {
        "object" => {
            if map.get("additionalProperties") != Some(&Value::Bool(false)) {
                return Err(Fault::new(
                    "Schema",
                    format!("{path}: additionalProperties must be false"),
                ));
            }
            let properties = map
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    Fault::new("Schema", format!("{path}: properties must be an object"))
                })?;
            let required = map
                .get("required")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    Fault::new("Schema", format!("{path}: required must be an array"))
                })?;
            let mut names = BTreeSet::new();
            for item in required {
                let name = item
                    .as_str()
                    .ok_or_else(|| Fault::new("Schema", "required entries must be strings"))?;
                if !properties.contains_key(name) || !names.insert(name) {
                    return Err(Fault::new(
                        "Schema",
                        format!("{path}: invalid or duplicate required field {name}"),
                    ));
                }
            }
            for (name, node) in properties {
                validate_schema(node, &format!("{path}.{name}"), false, depth + 1)?;
            }
        }
        "array" => {
            let items = map
                .get("items")
                .ok_or_else(|| Fault::new("Schema", format!("{path}: items required")))?;
            validate_schema(items, &format!("{path}[]"), false, depth + 1)?;
            validate_bounds(map, "minItems", "maxItems", true, path)?;
        }
        "string" => {
            validate_bounds(map, "minLength", "maxLength", true, path)?;
            if let Some(values) = map.get("enum") {
                let values = values
                    .as_array()
                    .filter(|items| !items.is_empty())
                    .ok_or_else(|| {
                        Fault::new("Schema", format!("{path}: enum must be nonempty"))
                    })?;
                let mut seen = BTreeSet::new();
                for value in values {
                    let value = value.as_str().ok_or_else(|| {
                        Fault::new("Schema", format!("{path}: enum entries must be strings"))
                    })?;
                    if !seen.insert(value) {
                        return Err(Fault::new(
                            "Schema",
                            format!("{path}: duplicate enum value"),
                        ));
                    }
                }
            }
        }
        "number" | "integer" => validate_bounds(map, "minimum", "maximum", false, path)?,
        _ => {}
    }
    if let Some(default) = map.get("default") {
        validate_value(schema, default, path)
            .map_err(|error| Fault::new("SchemaDefault", error.message))?;
    }
    Ok(())
}

fn validate_bounds(
    map: &Map<String, Value>,
    lower: &str,
    upper: &str,
    integral: bool,
    path: &str,
) -> Result<(), Fault> {
    for key in [lower, upper] {
        if let Some(value) = map.get(key) {
            if value.as_f64().is_none_or(|n| !n.is_finite())
                || (integral && value.as_u64().is_none())
            {
                return Err(Fault::new("Schema", format!("{path}: invalid {key}")));
            }
        }
    }
    if let (Some(low), Some(high)) = (
        map.get(lower).and_then(Value::as_f64),
        map.get(upper).and_then(Value::as_f64),
    ) {
        if low > high {
            return Err(Fault::new("Schema", format!("{path}: reversed bounds")));
        }
    }
    Ok(())
}

fn validate_value(schema: &Value, value: &Value, path: &str) -> Result<(), Fault> {
    let invalid = |detail: &str| Fault::new("Profile", format!("{path}: {detail}"));
    match schema["type"].as_str() {
        Some("object") => {
            let map = value
                .as_object()
                .ok_or_else(|| invalid("expected object"))?;
            let properties = schema["properties"]
                .as_object()
                .ok_or_else(|| invalid("invalid schema properties"))?;
            for key in schema["required"]
                .as_array()
                .ok_or_else(|| invalid("invalid required fields"))?
            {
                let key = key
                    .as_str()
                    .ok_or_else(|| invalid("invalid required field"))?;
                if !map.contains_key(key) {
                    return Err(Fault::new(
                        "Profile",
                        format!("{path}.{key}: required field missing"),
                    ));
                }
            }
            for (key, value) in map {
                let node = properties
                    .get(key)
                    .ok_or_else(|| Fault::new("Profile", format!("{path}.{key}: unknown field")))?;
                validate_value(node, value, &format!("{path}.{key}"))?;
            }
        }
        Some("array") => {
            let items = value.as_array().ok_or_else(|| invalid("expected array"))?;
            validate_length(schema, items.len(), "minItems", "maxItems", path)?;
            for (index, value) in items.iter().enumerate() {
                validate_value(&schema["items"], value, &format!("{path}[{index}]"))?;
            }
        }
        Some("string") => {
            let text = value.as_str().ok_or_else(|| invalid("expected string"))?;
            validate_length(schema, text.chars().count(), "minLength", "maxLength", path)?;
            if schema
                .get("enum")
                .and_then(Value::as_array)
                .is_some_and(|values| !values.contains(value))
            {
                return Err(invalid("value is not in enum"));
            }
        }
        Some(kind @ ("integer" | "number")) => {
            if kind == "integer" && value.as_i64().is_none() && value.as_u64().is_none() {
                return Err(invalid("expected integer"));
            }
            let number = value
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| invalid("expected finite number"))?;
            if schema
                .get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|low| number < low)
                || schema
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .is_some_and(|high| number > high)
            {
                return Err(invalid("number outside declared bounds"));
            }
        }
        Some("boolean") if value.is_boolean() => {}
        Some("boolean") => return Err(invalid("expected boolean")),
        _ => return Err(Fault::new("Schema", "unsupported schema type")),
    }
    Ok(())
}

fn validate_length(
    schema: &Value,
    length: usize,
    lower: &str,
    upper: &str,
    path: &str,
) -> Result<(), Fault> {
    if schema
        .get(lower)
        .and_then(Value::as_u64)
        .is_some_and(|n| (length as u64) < n)
        || schema
            .get(upper)
            .and_then(Value::as_u64)
            .is_some_and(|n| (length as u64) > n)
    {
        return Err(Fault::new(
            "Profile",
            format!("{path}: length outside declared bounds"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
