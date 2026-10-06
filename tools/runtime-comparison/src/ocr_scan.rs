//! Grouped Script OCR validation and the all-observations projection shared with trials.

use crate::model::{Fault, MAX_TRANSPORT_BYTES};
use crate::recognition::{GeometryBasis, NormalizedRect};
use crate::recognition_trial::{DIAGNOSTIC_BYTES, DIAGNOSTIC_REGIONS, OcrZone, TEXT_CONTRACT};
use serde::Deserialize;
use serde_json::{Value, json};

// A controlled-fixture ceiling, not a claim about native engine capabilities.
pub(crate) const CONTROLLED_MAX_ZONES: usize = DIAGNOSTIC_REGIONS;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub observation: Value,
    basis: GeometryBasis,
    zones: Vec<Zone>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Zone {
    id: String,
    region: NormalizedRect,
}

impl Request {
    pub(crate) fn into_zones(
        self,
        width: u32,
        height: u32,
        maximum: usize,
    ) -> Result<(Value, Vec<OcrZone>), Fault> {
        let invalid = |message| Fault::new("Argument", message);
        if self.zones.is_empty() || self.zones.len() > maximum {
            return Err(invalid(
                "OCR selection exceeds the grouped request capability",
            ));
        }
        if self.basis.frame_width != width || self.basis.frame_height != height {
            return Err(invalid(
                "recognition basis must match the retained capture extent",
            ));
        }
        self.basis
            .content
            .validate_in(width, height)
            .map_err(|fault| Fault::new("Argument", fault.message))?;
        for (index, zone) in self.zones.iter().enumerate() {
            if zone.id.is_empty()
                || zone.id.len() > 256
                || zone.id.chars().any(char::is_control)
                || self.zones[..index].iter().any(|prior| prior.id == zone.id)
            {
                return Err(invalid(
                    "selected zone IDs must be distinct nonempty bounded text",
                ));
            }
        }
        // Map the entire selection before any backend work. IDs and observation move
        // into the request/projection; no frame pixels or request JSON are copied.
        let zones = self
            .zones
            .into_iter()
            .map(|zone| {
                let rect = zone
                    .region
                    .map_in_content(self.basis.content)
                    .map_err(|fault| Fault::new("Argument", fault.message))?;
                Ok(OcrZone { id: zone.id, rect })
            })
            .collect::<Result<_, Fault>>()?;
        Ok((self.observation, zones))
    }
}

pub(crate) fn check_region_budget(count: usize, text_bytes: usize) -> Result<(), Fault> {
    if count > DIAGNOSTIC_REGIONS || text_bytes > DIAGNOSTIC_BYTES {
        return Err(Fault::new(
            "RecognitionOutputLimit",
            "recognition diagnostics exceed their finite projection budget",
        ));
    }
    Ok(())
}

pub(crate) fn region_value(text: &str, confidence: f64, points: [[f64; 2]; 4]) -> Value {
    let left = points.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    let top = points.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
    let right = points
        .iter()
        .map(|p| p[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = points
        .iter()
        .map(|p| p[1])
        .fold(f64::NEG_INFINITY, f64::max);
    json!({"text":text,"confidence":confidence,
        "bounds":{"x":left,"y":top,"width":right-left,"height":bottom-top},
        "geometry":points})
}

pub(crate) fn zone_value(id: String, regions: Vec<Value>) -> Value {
    let mut value = json!({"outcome":if regions.is_empty() {"no_match"} else {"recognized"}});
    value["id"] = Value::String(id);
    value["regions"] = Value::Array(regions);
    value
}

pub(crate) fn snapshot(
    observation: Option<Value>,
    zones: Vec<Value>,
    count: usize,
) -> Result<Value, Fault> {
    let mut value = json!({"kind":"ocr","text_contract":TEXT_CONTRACT});
    value["zones"] = Value::Array(zones);
    if let Some(observation) = observation {
        value["observation"] = observation;
    }
    // Count escaped JSON bytes without allocating a second serialized snapshot.
    struct ByteCount(usize);
    impl std::io::Write for ByteCount {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > DIAGNOSTIC_BYTES.min(MAX_TRANSPORT_BYTES) {
                return Err(std::io::Error::other("OCR snapshot byte bound exceeded"));
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    if count > DIAGNOSTIC_REGIONS || serde_json::to_writer(&mut ByteCount(0), &value).is_err() {
        return Err(Fault::new(
            "RecognitionOutputLimit",
            "recognition diagnostics exceed 256 regions or 256 KiB",
        ));
    }
    Ok(value)
}

#[cfg(feature = "engine")]
pub(crate) fn project(
    scanned: &mado_pilot::OcrZoneScanResult,
    zones: &[OcrZone],
    observation: Option<Value>,
) -> Result<Value, Fault> {
    let mut count = 0usize;
    let mut text_bytes = 0usize;
    let mut output = Vec::with_capacity(zones.len());
    for (index, zone) in zones.iter().enumerate() {
        let group = scanned.group(index).ok_or_else(|| {
            crate::environment::blocked("ocr_projection", "engine omitted selected OCR zone")
        })?;
        check_region_budget(count.saturating_add(group.len()), text_bytes)?;
        let mut regions = Vec::with_capacity(group.len());
        for region in group.iter() {
            count += 1;
            text_bytes = text_bytes.saturating_add(region.text().len());
            check_region_budget(count, text_bytes)?;
            regions.push(region_value(
                region.text(),
                region.confidence().get(),
                region
                    .geometry()
                    .points()
                    .map(|point| [point.x(), point.y()]),
            ));
        }
        output.push(zone_value(zone.id.clone(), regions));
    }
    snapshot(observation, output, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_mapping_uses_retained_extent_not_saved_image_budget() {
        let request: Request = serde_json::from_value(json!({
            "observation": {"id":"retained"},
            "basis": {"frame_width":6016,"frame_height":3384,
                "content":{"x":0,"y":0,"width":6016,"height":3384}},
            "zones": [{"id":"left","region":{"u0":0.0,"v0":0.0,"u1":0.5,"v1":0.5}}]
        }))
        .unwrap();
        assert!(
            request.basis.validate().is_err(),
            "saved image policy remains unchanged"
        );
        let (_, zones) = request.into_zones(6016, 3384, 1).unwrap();
        assert_eq!(
            zones[0].rect,
            crate::recognition::PixelRect {
                x: 0,
                y: 0,
                width: 3008,
                height: 1692,
            }
        );
    }

    #[test]
    fn grouped_snapshot_counts_escaped_bytes_and_observation_without_truncation() {
        let zone = json!({"id":"a","regions":[],"outcome":"no_match"});
        let empty = snapshot(Some(json!({"id":""})), vec![zone.clone()], 0).unwrap();
        let overhead = serde_json::to_vec(&empty).unwrap().len();
        let id = "x".repeat(DIAGNOSTIC_BYTES - overhead);
        let exact = snapshot(Some(json!({"id":id})), vec![zone.clone()], 0).unwrap();
        assert_eq!(serde_json::to_vec(&exact).unwrap().len(), DIAGNOSTIC_BYTES);
        assert_eq!(
            snapshot(Some(json!({"id":format!("{id}x")})), vec![zone.clone()], 0)
                .unwrap_err()
                .category,
            "RecognitionOutputLimit"
        );
        assert_eq!(
            snapshot(
                Some(json!({"id":"\0".repeat(DIAGNOSTIC_BYTES / 6)})),
                vec![zone],
                0
            )
            .unwrap_err()
            .category,
            "RecognitionOutputLimit"
        );
    }

    #[test]
    fn grouped_request_rejects_nonfinite_edges_before_projection() {
        for edge in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let request = Request {
                observation: Value::Null,
                basis: GeometryBasis {
                    frame_width: 20,
                    frame_height: 10,
                    content: crate::recognition::PixelRect {
                        x: 0,
                        y: 0,
                        width: 20,
                        height: 10,
                    },
                },
                zones: vec![Zone {
                    id: "a".into(),
                    region: NormalizedRect {
                        u0: 0.0,
                        v0: 0.0,
                        u1: edge,
                        v1: 1.0,
                    },
                }],
            };
            assert_eq!(
                request.into_zones(20, 10, 1).unwrap_err().category,
                "Argument"
            );
        }
    }
}
