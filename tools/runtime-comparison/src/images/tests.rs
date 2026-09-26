use super::*;
use flate2::write::ZlibEncoder;

fn append_chunk(output: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(name);
    output.extend_from_slice(data);
    let mut crc = !0u32;
    for byte in name.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    output.extend_from_slice(&(!crc).to_be_bytes());
}

fn compressed(rows: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(rows).unwrap();
    encoder.finish().unwrap()
}

fn png_with_data(
    width: u32,
    height: u32,
    depth: u8,
    color: u8,
    interlaced: u8,
    extra: &[(&[u8; 4], &[u8])],
    data: &[u8],
) -> Vec<u8> {
    let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[depth, color, 0, 0, interlaced]);
    append_chunk(&mut output, b"IHDR", &header);
    for (name, bytes) in extra {
        append_chunk(&mut output, name, bytes);
    }
    append_chunk(&mut output, b"IDAT", data);
    append_chunk(&mut output, b"IEND", &[]);
    output
}

#[test]
fn refuses_corrupt_pixels_checksums_and_incomplete_or_surplus_streams() {
    let zlib = compressed(&[0, 11, 22, 33, 255]);
    let valid = png_with_data(1, 1, 8, 6, 0, &[], &zlib);
    assert_eq!(
        decode_png(&valid, ImageKind::Input).unwrap().rgba,
        [11, 22, 33, 255]
    );
    let mut bad_crc = valid.clone();
    bad_crc[29] ^= 1;
    let mut after_end = valid.clone();
    after_end.push(0);
    let mut compressed_tail = zlib.clone();
    compressed_tail.push(0);
    let mut bad_adler = zlib.clone();
    *bad_adler.last_mut().unwrap() ^= 1;
    let mut bad_ancillary = png_with_data(1, 1, 8, 6, 0, &[(b"tEXt", b"key\0value")], &zlib);
    bad_ancillary[41] ^= 1;
    let cases = [
        ("IHDR-only", valid[..33].to_vec()),
        ("chunk CRC", bad_crc),
        ("ancillary CRC", bad_ancillary),
        ("missing IEND", valid[..valid.len() - 12].to_vec()),
        ("trailing bytes", after_end),
        (
            "invalid filter",
            png_with_data(1, 1, 8, 6, 0, &[], &compressed(&[5, 11, 22, 33, 255])),
        ),
        (
            "short scanline",
            png_with_data(1, 1, 8, 6, 0, &[], &compressed(&[0, 11])),
        ),
        (
            "surplus scanline",
            png_with_data(1, 1, 8, 6, 0, &[], &compressed(&[0, 11, 22, 33, 255, 0])),
        ),
        (
            "missing zlib trailer",
            png_with_data(1, 1, 8, 6, 0, &[], &zlib[..zlib.len() - 4]),
        ),
        (
            "Adler checksum",
            png_with_data(1, 1, 8, 6, 0, &[], &bad_adler),
        ),
        (
            "compressed tail",
            png_with_data(1, 1, 8, 6, 0, &[], &compressed_tail),
        ),
        (
            "missing palette",
            png_with_data(1, 1, 8, 3, 0, &[], &compressed(&[0, 0])),
        ),
        (
            "partial palette entry",
            png_with_data(1, 1, 8, 3, 0, &[(b"PLTE", &[1, 2])], &compressed(&[0, 0])),
        ),
        (
            "absent palette index",
            png_with_data(
                1,
                1,
                8,
                3,
                0,
                &[(b"PLTE", &[1, 2, 3])],
                &compressed(&[0, 1]),
            ),
        ),
        (
            "surplus palette alpha",
            png_with_data(
                1,
                1,
                8,
                3,
                0,
                &[(b"PLTE", &[1, 2, 3]), (b"tRNS", &[0, 0])],
                &compressed(&[0, 0]),
            ),
        ),
        (
            "animation",
            png_with_data(
                1,
                1,
                8,
                6,
                0,
                &[(b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0])],
                &zlib,
            ),
        ),
    ];
    for (name, bytes) in cases {
        assert!(validate_png(&bytes, ImageKind::Input).is_err(), "{name}");
        assert!(decode_png(&bytes, ImageKind::Input).is_err(), "{name}");
    }
}

#[test]
fn tiny_compressed_image_cannot_claim_oversized_geometry() {
    let bytes = png_with_data(4096, 4097, 8, 6, 0, &[], &compressed(&[0]));
    let error = validate_png(&bytes, ImageKind::Input).unwrap_err();
    assert_eq!(error.category, "ImageLimit");
    assert_eq!(error.context["resource"], "image pixels");
    let crop = png_with_data(2048, 2049, 8, 6, 0, &[], &compressed(&[0]));
    assert_eq!(
        validate_png(&crop, ImageKind::Crop).unwrap_err().category,
        "ImageLimit"
    );
}

#[test]
fn expands_palette_transparency_and_strips_sixteen_bit_samples() {
    let indexed = png_with_data(
        2,
        1,
        1,
        3,
        0,
        &[(b"PLTE", &[255, 0, 0, 0, 255, 0]), (b"tRNS", &[0, 128])],
        &compressed(&[0, 0b0100_0000]),
    );
    assert_eq!(
        decode_png(&indexed, ImageKind::Input).unwrap().rgba,
        [255, 0, 0, 0, 0, 255, 0, 128]
    );
    let gray = png_with_data(
        2,
        1,
        16,
        0,
        0,
        &[],
        &compressed(&[0, 0x12, 0x34, 0xab, 0xcd]),
    );
    assert_eq!(
        decode_png(&gray, ImageKind::Input).unwrap().rgba,
        [0x12, 0x12, 0x12, 255, 0xab, 0xab, 0xab, 255]
    );
}

#[test]
fn deinterlaces_and_crops_original_pixels_without_metadata() {
    // A 2x2 Adam7 image has one sample in passes 1 and 6, then two in pass 7.
    let bytes = png_with_data(
        2,
        2,
        8,
        6,
        1,
        &[(b"tEXt", b"private\0discard")],
        &compressed(&[
            0, 10, 20, 30, 255, 0, 40, 50, 60, 255, 0, 70, 80, 90, 255, 100, 110, 120, 255,
        ]),
    );
    let image = decode_png(&bytes, ImageKind::Input).unwrap();
    assert_eq!(
        image.rgba,
        [
            10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255
        ]
    );
    let encoded = encode_crop(&image, [1, 0, 1, 2]).unwrap();
    let crop = decode_png(encoded.as_bytes(), ImageKind::Crop).unwrap();
    assert_eq!((crop.width, crop.height), (1, 2));
    assert_eq!(crop.rgba, [40, 50, 60, 255, 100, 110, 120, 255]);
    let mut offset = 8;
    while offset < encoded.as_bytes().len() {
        let (name, _, next) = chunk(encoded.as_bytes(), offset).unwrap();
        assert!(matches!(name, b"IHDR" | b"IDAT" | b"IEND"));
        offset = next;
    }
    assert!(encode_crop(&image, [1, 0, 2, 2]).is_err());
    assert!(encode_crop(&image, [u32::MAX, 0, 1, 1]).is_err());
}

#[test]
fn aggregate_reservation_follows_shared_owner_and_refuses_overcommit() {
    let budget = PayloadBudget::default();
    let occupied = budget.reserve(PAYLOAD_BYTES - 8).unwrap();
    let bytes = vec![1; 8];
    let owner = PayloadBytes::from_reserved(bytes, budget.reserve(8).unwrap()).unwrap();
    let second_owner = owner.clone();
    drop(owner);
    assert!(budget.reserve(1).is_err());
    assert_eq!(budget.used(), PAYLOAD_BYTES);
    drop(second_owner);
    assert_eq!(budget.used(), PAYLOAD_BYTES - 8);
    let mut remainder = budget.reserve(8).unwrap();
    assert!(remainder.resize(9).is_err());
    drop(occupied);
    remainder.resize(4).unwrap();
    assert_eq!(budget.used(), 4);
    drop(remainder);
    assert_eq!(budget.used(), 0);
}

#[test]
fn preview_changes_only_display_raster() {
    let width = 4096;
    let height = 2048;
    let length = checked_rgba_bytes(width, height, ImageKind::Input).unwrap();
    let owner = reserve_payload(length).unwrap();
    let mut pixels = vec![0; length];
    pixels[..4].copy_from_slice(&[12, 34, 56, 255]);
    let frame = DecodedImage::from_reserved_rgba(width, height, pixels, owner).unwrap();
    let display = preview(&frame).unwrap();
    assert!(display.rgba.len() <= CROP_MAX_PIXELS * 4);
    assert!(display.width < width && display.height < height);
    assert_eq!(&display.rgba[..4], &[12, 34, 56, 255]);
    assert_eq!(
        (frame.width, frame.height, frame.rgba.len()),
        (width, height, length)
    );
    assert_eq!(&frame.rgba[..4], &[12, 34, 56, 255]);
}
