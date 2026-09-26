//! Finite metadata stays JSON; captured asset bytes cross the child boundary exactly once.
use super::protocol::{Invocation, frame};
use crate::environment::Configuration;
use crate::images::{self, PayloadBytes, PayloadReservation};
use crate::inventory::Inventory;
use crate::model::{Fault, MAX_TRANSPORT_BYTES, Plan, encode_bounded};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};

#[derive(Serialize)]
struct Header<'a> {
    version: u32,
    invocation: &'a Invocation,
    asset_lengths: BTreeMap<&'a str, usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Incoming {
    version: u32,
    invocation: Invocation,
    asset_lengths: BTreeMap<String, usize>,
}

pub(super) fn header(invocation: &mut Invocation) -> Result<Vec<u8>, Fault> {
    let assets = std::mem::take(&mut invocation.inventory.assets);
    let result = encode_bounded(
        &Header {
            version: 1,
            invocation,
            asset_lengths: assets
                .iter()
                .map(|(name, bytes)| (name.as_str(), bytes.len()))
                .collect(),
        },
        MAX_TRANSPORT_BYTES - 1,
    );
    invocation.inventory.assets = assets;
    let mut bytes = result?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(super) fn write(invocation: &mut Invocation, output: &mut impl Write) -> Result<(), Fault> {
    output.write_all(&header(invocation)?).map_err(transport)?;
    for bytes in invocation.inventory.assets.values() {
        output.write_all(bytes).map_err(transport)?;
    }
    output.flush().map_err(transport)
}

pub(super) fn read(input: &mut impl BufRead) -> Result<Invocation, Fault> {
    let bytes = frame(input, MAX_TRANSPORT_BYTES)?
        .ok_or_else(|| Fault::new("Transport", "missing binary invocation header"))?;
    let Incoming {
        version,
        mut invocation,
        asset_lengths,
    } = serde_json::from_slice(&bytes)
        .map_err(|_| Fault::new("Transport", "invalid binary invocation header"))?;
    if version != 1
        || !invocation.inventory.assets.is_empty()
        || asset_lengths.len() > invocation.plan.limits.snapshot_files
        || asset_lengths
            .keys()
            .any(|key| key.is_empty() || key.len() > 240)
    {
        return Err(Fault::new("Transport", "invalid binary asset manifest"));
    }
    invocation.plan.validate()?;
    let mut total = 0usize;
    for size in asset_lengths.values() {
        total = total
            .checked_add(*size)
            .filter(|total| *total <= images::PACKAGE_BYTES)
            .ok_or_else(|| {
                Fault::new(
                    "LimitExceeded",
                    "binary assets exceed the captured package ceiling",
                )
            })?;
    }
    for (name, size) in asset_lengths {
        let reservation = images::reserve_payload(size)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| Fault::new("LimitExceeded", "binary asset allocation failed"))?;
        bytes.resize(size, 0);
        input.read_exact(&mut bytes).map_err(transport)?;
        invocation
            .inventory
            .assets
            .insert(name, PayloadBytes::from_reserved(bytes, reservation)?);
    }
    Ok(invocation)
}

fn transport(_: std::io::Error) -> Fault {
    Fault::new("Transport", "binary asset transfer was incomplete")
}

/// Host reservation covers child-held assets and public-facade owned image copies
/// until reaping. It does not claim a bound for native library/model internal RSS.
pub(super) fn reserve_child_images(
    plan: &Plan,
    inventory: &Inventory,
) -> Result<PayloadReservation, Fault> {
    let mut bytes = inventory
        .assets
        .values()
        .try_fold(0usize, |sum, value| add(sum, value.len()))?;
    bytes = add(bytes, inventory.png_validation_scratch_bytes()?)?;
    if plan.lane == "controlled" {
        return images::reserve_payload(bytes);
    }
    let Some(raw) = &plan.native_config else {
        return images::reserve_payload(bytes);
    };
    let config: Configuration<Value> = serde_json::from_value(raw.clone()).map_err(|_| {
        Fault::new(
            "Blocked",
            "invalid engine configuration for image accounting",
        )
    })?;
    let (entries, aliases): (BTreeMap<String, String>, BTreeMap<String, String>) =
        if let Some(replay) = &config.replay {
            let mut held = 0usize;
            let mut largest = 0usize;
            for frame in &replay.frames {
                let size = images::checked_rgba_bytes(
                    frame.width,
                    frame.height,
                    images::ImageKind::Input,
                )?;
                held = add(held, size)?;
                largest = largest.max(size);
            }
            if held > images::REPLAY_DECODED_BYTES {
                return Err(Fault::new(
                    "LimitExceeded",
                    "decoded replay frames exceed their aggregate ceiling",
                ));
            }
            // ReplaySource and ReplaySession own separate copies; mapping, BGR search,
            // and f32 correlation response are simultaneous at the largest frame size.
            bytes = add(bytes, held.checked_mul(2).ok_or_else(overflow)?)?;
            bytes = add(bytes, largest.checked_mul(11).ok_or_else(overflow)? / 4)?;
            (replay.package_entries.clone(), replay.templates.clone())
        } else if let Some(native) = &config.native {
            // Native frame acquisition retains its existing separately authorized bounds.
            let entries = serde_json::from_value(
                native
                    .get("package_entries")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            )
            .map_err(|_| Fault::new("Blocked", "invalid native package mapping"))?;
            let aliases = serde_json::from_value(
                native
                    .get("templates")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            )
            .map_err(|_| Fault::new("Blocked", "invalid native template mapping"))?;
            (entries, aliases)
        } else {
            return images::reserve_payload(bytes);
        };
    for asset in entries.values() {
        let payload = inventory.assets.get(asset).ok_or_else(|| {
            Fault::new(
                "Blocked",
                "template mapping references an absent captured asset",
            )
        })?;
        bytes = add(bytes, payload.len())?;
    }
    if !aliases.is_empty() {
        let manifest = entries
            .get("madopilot-package.json")
            .and_then(|id| inventory.assets.get(id))
            .ok_or_else(|| {
                Fault::new(
                    "Blocked",
                    "template manifest is absent from captured mappings",
                )
            })?;
        let manifest: Value = serde_json::from_slice(manifest)
            .map_err(|_| Fault::new("Blocked", "template manifest is not valid JSON"))?;
        let templates = manifest["templates"]
            .as_array()
            .ok_or_else(|| Fault::new("Blocked", "template manifest contains no declarations"))?;
        for id in aliases.values() {
            let template = templates
                .iter()
                .find(|template| template["id"] == *id)
                .ok_or_else(|| {
                    Fault::new("Blocked", "template alias has no manifest declaration")
                })?;
            let path = template["path"]
                .as_str()
                .ok_or_else(|| Fault::new("Blocked", "template path is invalid"))?;
            let payload = entries
                .get(path)
                .and_then(|asset| inventory.assets.get(asset))
                .ok_or_else(|| Fault::new("Blocked", "template pixels are not captured"))?;
            let info = images::validate_png(payload, images::ImageKind::Input)?;
            bytes = add(bytes, info.rgba_bytes / 4 * 3)?;
            bytes = add(
                bytes,
                images::png_scratch_bytes(payload, images::ImageKind::Input)?,
            )?;
        }
    }
    images::reserve_payload(bytes)
}

fn add(left: usize, right: usize) -> Result<usize, Fault> {
    left.checked_add(right).ok_or_else(overflow)
}

fn overflow() -> Fault {
    Fault::new("LimitExceeded", "owned child image payload size overflow")
}

#[cfg(test)]
mod tests {
    use super::super::protocol::Operation;
    use super::*;
    use std::io::Cursor;

    fn invocation() -> Invocation {
        let mut plan: Plan =
            serde_json::from_str(include_str!("../../fixtures/manual-plan.json")).unwrap();
        plan.limits.snapshot_bytes = images::PACKAGE_BYTES;
        let inventory = Inventory::capture(
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/typescript")),
            &plan.limits,
        )
        .unwrap();
        Invocation {
            run: "binary-regression".into(),
            attempt: 1,
            operation: Operation::Run,
            plan,
            inventory,
            observe_logs: false,
        }
    }

    #[test]
    fn image_sized_binary_segments_cannot_consume_the_following_stop_frame() {
        let mut invocation = invocation();
        // Above the previous decoded-frame allowance; newline/control-looking bytes
        // are pixels, not control records.
        let mut image = vec![10u8; 3 * 1024 * 1024];
        image[..4].copy_from_slice(b"Stop");
        invocation
            .inventory
            .assets
            .insert("marker".into(), PayloadBytes::new(image).unwrap());
        let mut wire = Vec::new();
        write(&mut invocation, &mut wire).unwrap();
        wire.extend_from_slice(b"{\"command\":\"Stop\"}\n");
        let mut input = Cursor::new(wire);
        let received = read(&mut input).unwrap();
        assert_eq!(
            received.inventory.assets["marker"],
            invocation.inventory.assets["marker"]
        );
        let command: Value =
            serde_json::from_slice(&frame(&mut input, 1024).unwrap().unwrap()).unwrap();
        assert_eq!(command, serde_json::json!({"command":"Stop"}));
    }

    #[test]
    fn oversized_binary_lengths_are_rejected_before_reading_or_allocating_payloads() {
        let mut invocation = invocation();
        let mut value: Value = serde_json::from_slice(&header(&mut invocation).unwrap()).unwrap();
        value["asset_lengths"]["marker"] = serde_json::json!(images::PACKAGE_BYTES + 1);
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        let error = read(&mut Cursor::new(bytes))
            .err()
            .expect("oversized payload refused");
        assert_eq!(error.category, "LimitExceeded");
    }

    #[test]
    fn truncated_binary_input_never_publishes_a_partial_inventory() {
        let mut invocation = invocation();
        let bytes = header(&mut invocation).unwrap();
        let error = read(&mut Cursor::new(bytes))
            .err()
            .expect("missing binary data refused");
        assert_eq!(error.category, "Transport");
    }
}
