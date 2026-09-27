use super::{Inventory, PackageDraft};
use crate::images::{self, ImageKind, PayloadBytes};
use crate::model::{Limits, Plan};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

fn limits() -> Limits {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../fixtures/controlled-plan.json")).unwrap();
    plan.limits.snapshot_bytes = images::PACKAGE_BYTES;
    plan.limits
}

fn files() -> BTreeMap<String, PayloadBytes> {
    PackageDraft::capture(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
        &limits(),
    )
    .unwrap()
    .files()
    .clone()
}

fn add_asset(
    files: &mut BTreeMap<String, PayloadBytes>,
    id: &str,
    format: &str,
    width: u32,
    height: u32,
    bytes: PayloadBytes,
) {
    let path = format!("assets/{id}.data");
    let mut manifest: Value = serde_json::from_slice(&files["package.json"]).unwrap();
    manifest["assets"][id] = json!({"path":path,"format":format,"width":width,"height":height});
    files.insert(
        "package.json".into(),
        PayloadBytes::new(serde_json::to_vec(&manifest).unwrap()).unwrap(),
    );
    files.insert(path, bytes);
}

fn inventory(
    files: BTreeMap<String, PayloadBytes>,
    bounds: &Limits,
) -> Result<Inventory, crate::model::Fault> {
    super::capture::inventory_from_files(files, bounds)
}

fn solid_png(width: u32, height: u32) -> PayloadBytes {
    use std::io::Write;
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        {
            let mut stream = writer.stream_writer().unwrap();
            let row = vec![0; width as usize * 4];
            for _ in 0..height {
                stream.write_all(&row).unwrap();
            }
            stream.finish().unwrap();
        }
        writer.finish().unwrap();
    }
    PayloadBytes::new(bytes).unwrap()
}

#[test]
fn image_bytes_do_not_consume_source_allowance_but_explicit_lower_limit_still_applies() {
    let mut package = files();
    let pixels = PayloadBytes::new(vec![0; 513 * 513 * 4]).unwrap();
    add_asset(&mut package, "large", "raw-rgba8", 513, 513, pixels.clone());
    let draft = PackageDraft::from_files(package.clone(), &limits()).unwrap();
    let captured = draft.validate().unwrap();
    assert_eq!(captured.assets["large"].as_slice(), pixels.as_ref());
    assert_eq!(
        captured.metadata["capture_limits"]["bytes"],
        images::PACKAGE_BYTES
    );
    captured.validate().unwrap();
    let mut smaller = limits();
    smaller.snapshot_bytes = 1024 * 1024;
    assert!(PackageDraft::from_files(package.clone(), &smaller).is_err());
    assert!(inventory(package, &smaller).is_err());
}

#[test]
fn full_resolution_raw_frame_is_identified_within_the_normal_capture_bound() {
    let mut package = files();
    add_asset(
        &mut package,
        "frame",
        "raw-rgba8",
        3840,
        2160,
        PayloadBytes::new(vec![0; 3840 * 2160 * 4]).unwrap(),
    );
    let mut bounds = limits();
    bounds.duration_ms = 10_000;
    let mut captured = inventory(package, &bounds).unwrap();
    let original = captured.identity.clone();
    let mut changed = captured.assets["frame"].as_slice().to_vec();
    *changed.last_mut().unwrap() = 1;
    captured
        .assets
        .insert("frame".into(), PayloadBytes::new(changed).unwrap());
    assert!(
        captured.validate().is_err(),
        "an unchanged identity must reject changed pixels"
    );
    captured.refresh_identity().unwrap();
    assert_ne!(captured.identity, original);
    captured.validate().unwrap();
}

#[test]
fn metadata_sources_and_profiles_cannot_borrow_image_allowance() {
    let base = files();
    let manifest: Value = serde_json::from_slice(&base["package.json"]).unwrap();
    let source = manifest["sources"][0].as_str().unwrap();
    let profile = manifest["profiles"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .as_str()
        .unwrap();
    for path in [source, profile] {
        let mut package = base.clone();
        package.insert(
            path.into(),
            PayloadBytes::new(vec![b' '; images::PACKAGE_NON_IMAGE_BYTES + 1]).unwrap(),
        );
        let fault = PackageDraft::from_files(package.clone(), &limits()).unwrap_err();
        assert!(fault.message.contains("non-image"), "{fault}");
        let fault = inventory(package, &limits()).unwrap_err();
        assert!(fault.message.contains("non-image"), "{fault}");
    }
    let mut package = base;
    let metadata =
        serde_json::to_vec(&json!({"text":"x".repeat(images::PACKAGE_NON_IMAGE_BYTES)})).unwrap();
    add_asset(
        &mut package,
        "metadata",
        "json",
        0,
        0,
        PayloadBytes::new(metadata).unwrap(),
    );
    assert!(
        PackageDraft::from_files(package.clone(), &limits())
            .unwrap_err()
            .message
            .contains("non-image")
    );
    assert!(
        inventory(package, &limits())
            .unwrap_err()
            .message
            .contains("non-image")
    );
}

#[test]
fn cumulative_decoded_assets_refuse_small_compressed_packages_before_retaining_pixels() {
    let mut package = files();
    let bytes = solid_png(4096, 4096);
    assert!(bytes.len() < images::INPUT_MAX_BYTES);
    // Each asset is independently within 64MiB RGBA8; the third exceeds 128MiB.
    for id in ["frame_a", "frame_b", "frame_c"] {
        add_asset(&mut package, id, "png", 4096, 4096, bytes.clone());
    }
    let draft_error = PackageDraft::from_files(package.clone(), &limits()).unwrap_err();
    assert!(
        draft_error.message.contains("decoded image"),
        "{draft_error}"
    );
    let inventory_error = inventory(package, &limits()).unwrap_err();
    assert!(
        inventory_error.message.contains("decoded image"),
        "{inventory_error}"
    );
}

#[test]
fn aggregate_image_bytes_cannot_borrow_unused_non_image_allowance() {
    let mut package = files();
    let bytes = PayloadBytes::new(vec![0; 1024 * 1024 * 4]).unwrap();
    // Sixteen distinct declared assets use 64MiB; the fixture's retained marker
    // then exceeds image bytes while total package bytes still fit within 65MiB.
    for index in 0..16 {
        add_asset(
            &mut package,
            &format!("image{index:02}"),
            "raw-rgba8",
            1024,
            1024,
            bytes.clone(),
        );
    }
    let draft_error = PackageDraft::from_files(package.clone(), &limits()).unwrap_err();
    assert!(
        draft_error.message.contains("image byte limit"),
        "{draft_error}"
    );
    let inventory_error = inventory(package, &limits()).unwrap_err();
    assert!(
        inventory_error.message.contains("image byte limit"),
        "{inventory_error}"
    );
}

#[test]
fn full_png_validation_is_required_for_drafts_and_inventory_refresh() {
    let valid = solid_png(2, 2);
    let mut package = files();
    add_asset(&mut package, "png", "png", 2, 2, valid.clone());
    let mut captured = inventory(package.clone(), &limits()).unwrap();
    assert_eq!(
        images::validate_png(&captured.assets["png"], ImageKind::Input)
            .unwrap()
            .rgba_bytes,
        16
    );
    // Preserve the old accepted header/metadata but remove the required terminator.
    let broken = PayloadBytes::new(valid[..valid.len() - 12].to_vec()).unwrap();
    add_asset(&mut package, "png", "png", 2, 2, broken.clone());
    assert!(PackageDraft::from_files(package.clone(), &limits()).is_err());
    assert!(inventory(package, &limits()).is_err());
    captured.assets.insert("png".into(), broken);
    assert!(captured.refresh_identity().is_err());
}

#[test]
fn legal_package_ceiling_does_not_relax_other_plan_bounds() {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../fixtures/controlled-plan.json")).unwrap();
    plan.limits.snapshot_bytes = images::PACKAGE_BYTES;
    plan.validate().unwrap();
    plan.limits.snapshot_bytes += 1;
    assert!(plan.validate().is_err());
    plan.limits.snapshot_bytes = images::PACKAGE_BYTES;
    plan.limits.snapshot_files = 1025;
    assert!(plan.validate().is_err());
}

#[test]
fn recovery_capture_admits_two_revisions_but_keeps_the_union_finite() {
    struct Temporary(std::path::PathBuf);
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Temporary(std::env::temp_dir().join(format!(
        "mado-image-recovery-{}-{nonce}",
        std::process::id()
    )));
    std::fs::create_dir(&root.0).unwrap();
    let mut bounds = limits();
    bounds.snapshot_files = 2;
    bounds.snapshot_bytes = 2048;
    let mut expected = BTreeMap::new();
    for index in 0..4u8 {
        let path = format!("part{index}.bin");
        let bytes = vec![index; 1024];
        std::fs::write(root.0.join(&path), &bytes).unwrap();
        expected.insert(path, PayloadBytes::new(bytes).unwrap());
    }
    assert!(super::capture::capture_files(&root.0, &bounds, None).is_err());
    assert_eq!(
        PackageDraft::capture_recovery_bytes(&root.0, &bounds).unwrap(),
        expected
    );
    std::fs::write(root.0.join("surplus.bin"), [0]).unwrap();
    assert!(PackageDraft::capture_recovery_bytes(&root.0, &bounds).is_err());
    std::fs::remove_file(root.0.join("surplus.bin")).unwrap();
    std::fs::write(root.0.join("part0.bin"), vec![0; 1025]).unwrap();
    assert!(PackageDraft::capture_recovery_bytes(&root.0, &bounds).is_err());
}
