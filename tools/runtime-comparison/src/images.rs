//! Shared finite image policy. Payload reservations cover image buffers, not process RSS.
use crate::model::Fault;
use flate2::{Decompress, FlushDecompress, Status};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};

pub const INPUT_MAX_BYTES: usize = 32 * 1024 * 1024;
pub const INPUT_MAX_PIXELS: usize = 16_777_216;
pub const CROP_MAX_BYTES: usize = 16 * 1024 * 1024;
pub const CROP_MAX_PIXELS: usize = 4_194_304;
pub const MAX_SIDE: u32 = 16_384;
pub const PACKAGE_IMAGE_BYTES: usize = 64 * 1024 * 1024;
pub const PACKAGE_NON_IMAGE_BYTES: usize = 1024 * 1024;
pub const PACKAGE_BYTES: usize = PACKAGE_IMAGE_BYTES + PACKAGE_NON_IMAGE_BYTES;
pub const PACKAGE_DECODED_BYTES: usize = 128 * 1024 * 1024;
pub const REPLAY_DECODED_BYTES: usize = 128 * 1024 * 1024;
pub const PAYLOAD_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Input,
    Crop,
}

impl ImageKind {
    fn compressed_limit(self) -> usize {
        match self {
            Self::Input => INPUT_MAX_BYTES,
            Self::Crop => CROP_MAX_BYTES,
        }
    }
    fn pixel_limit(self) -> usize {
        match self {
            Self::Input => INPUT_MAX_PIXELS,
            Self::Crop => CROP_MAX_PIXELS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub rgba_bytes: usize,
}

/// One shared counter per application. A child copy must be reserved in the host
/// before launch, with that reservation retained until physical child settlement.
#[derive(Clone, Debug)]
struct PayloadBudget(Arc<AtomicUsize>);

impl Default for PayloadBudget {
    fn default() -> Self {
        Self(Arc::new(AtomicUsize::new(0)))
    }
}

impl PayloadBudget {
    fn reserve(&self, bytes: usize) -> Result<PayloadReservation, Fault> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                held.checked_add(bytes)
                    .filter(|total| *total <= PAYLOAD_BYTES)
            })
            .map_err(|_| limit("aggregate image payload bytes", PAYLOAD_BYTES))?;
        Ok(PayloadReservation {
            budget: self.clone(),
            bytes,
        })
    }

    #[cfg(test)]
    fn used(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }
}

/// Non-cloneable ownership of a reservation; sharing the payload must share its
/// owner (for example with Arc), while copying it requires a new reservation.
#[derive(Debug)]
pub struct PayloadReservation {
    budget: PayloadBudget,
    bytes: usize,
}

impl PayloadReservation {
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn resize(&mut self, bytes: usize) -> Result<(), Fault> {
        if bytes > self.bytes {
            let mut additional = self.budget.reserve(bytes - self.bytes)?;
            additional.bytes = 0;
        } else {
            self.budget
                .0
                .fetch_sub(self.bytes - bytes, Ordering::AcqRel);
        }
        self.bytes = bytes;
        Ok(())
    }
}

impl Drop for PayloadReservation {
    fn drop(&mut self) {
        self.budget.0.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

pub fn reserve_payload(bytes: usize) -> Result<PayloadReservation, Fault> {
    static BUDGET: LazyLock<PayloadBudget> = LazyLock::new(PayloadBudget::default);
    BUDGET.reserve(bytes)
}

/// Immutable package bytes carry their reservation through every shared clone.
/// Captured source/JSON bytes are conservatively charged while held in this form.
#[derive(Clone, Debug)]
pub struct PayloadBytes(Arc<ReservedBytes>);

#[derive(Debug)]
struct ReservedBytes {
    bytes: Vec<u8>,
    _reservation: PayloadReservation,
    digest: OnceLock<sha2::digest::Output<Sha256>>,
}

impl PayloadBytes {
    pub fn new(bytes: Vec<u8>) -> Result<Self, Fault> {
        let reservation = reserve_payload(bytes.capacity())?;
        Self::from_reserved(bytes, reservation)
    }

    pub fn from_reserved(bytes: Vec<u8>, reservation: PayloadReservation) -> Result<Self, Fault> {
        if bytes.len() > PACKAGE_BYTES || reservation.bytes() < bytes.capacity() {
            return Err(invalid("package payload length or reservation is invalid"));
        }
        Ok(Self(Arc::new(ReservedBytes {
            bytes,
            _reservation: reservation,
            digest: OnceLock::new(),
        })))
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0.bytes
    }

    pub(crate) fn digest(&self) -> &sha2::digest::Output<Sha256> {
        self.0.digest.get_or_init(|| Sha256::digest(&self.0.bytes))
    }

    pub(crate) fn into_text(self) -> Result<String, Fault> {
        match Arc::try_unwrap(self.0) {
            Ok(owner) => {
                String::from_utf8(owner.bytes).map_err(|_| invalid("package text is not UTF-8"))
            }
            Err(owner) => std::str::from_utf8(&owner.bytes)
                .map(str::to_owned)
                .map_err(|_| invalid("package text is not UTF-8")),
        }
    }
}

impl std::ops::Deref for PayloadBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for PayloadBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl PartialEq for PayloadBytes {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}
impl Eq for PayloadBytes {}

impl Serialize for PayloadBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_slice().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PayloadBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BytesVisitor;
        impl<'de> serde::de::Visitor<'de> for BytesVisitor {
            type Value = PayloadBytes;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded package byte array")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut bytes = Vec::new();
                let mut reservation = reserve_payload(0).map_err(serde::de::Error::custom)?;
                while let Some(byte) = sequence.next_element::<u8>()? {
                    let length = bytes.len() + 1;
                    grow_payload(&mut bytes, &mut reservation, length, PACKAGE_BYTES)
                        .map_err(serde::de::Error::custom)?;
                    bytes.push(byte);
                }
                PayloadBytes::from_reserved(bytes, reservation).map_err(serde::de::Error::custom)
            }
        }
        deserializer.deserialize_seq(BytesVisitor)
    }
}

fn grow_payload(
    bytes: &mut Vec<u8>,
    reservation: &mut PayloadReservation,
    length: usize,
    maximum: usize,
) -> Result<(), Fault> {
    if length > maximum {
        return Err(limit("encoded payload bytes", maximum));
    }
    if length > bytes.capacity() {
        let capacity = length.max(bytes.capacity().saturating_mul(2)).min(maximum);
        // A realloc may briefly retain both old and new allocations.
        let _old = reservation.budget.reserve(bytes.capacity())?;
        reservation.resize(capacity)?;
        bytes
            .try_reserve_exact(capacity - bytes.len())
            .map_err(|_| limit("image allocation bytes", capacity))?;
    }
    Ok(())
}

/// Reservation required for the pinned decoder, excluding borrowed input and
/// returned pixels. This preflight is not a substitute for full PNG validation.
pub fn png_scratch_bytes(bytes: &[u8], kind: ImageKind) -> Result<usize, Fault> {
    Ok(inspect_envelope(bytes, kind)?.scratch_bytes())
}

#[derive(Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    reservation: PayloadReservation,
}

impl DecodedImage {
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, Fault> {
        let reservation = reserve_payload(rgba.capacity())?;
        Self::from_reserved_rgba(width, height, rgba, reservation)
    }

    /// Reserve before reading/allocating a transferred frame, then move both
    /// the pixels and their reservation here without copying or double charging.
    pub fn from_reserved_rgba(
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        reservation: PayloadReservation,
    ) -> Result<Self, Fault> {
        if rgba.len() != checked_rgba_bytes(width, height, ImageKind::Input)?
            || reservation.bytes() < rgba.capacity()
        {
            return Err(invalid(
                "RGBA8 length or payload reservation disagrees with dimensions",
            ));
        }
        Ok(Self {
            width,
            height,
            rgba,
            reservation,
        })
    }

    pub fn into_parts(self) -> (Vec<u8>, PayloadReservation) {
        (self.rgba, self.reservation)
    }

    fn validate(&self) -> Result<(), Fault> {
        if self.rgba.len() != checked_rgba_bytes(self.width, self.height, ImageKind::Input)?
            || self.rgba.capacity() > self.reservation.bytes()
        {
            return Err(invalid("RGBA8 pixels changed outside their reserved owner"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct EncodedImage {
    bytes: Vec<u8>,
    reservation: PayloadReservation,
}

impl EncodedImage {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_parts(self) -> (Vec<u8>, PayloadReservation) {
        (self.bytes, self.reservation)
    }
}

pub fn checked_rgba_bytes(width: u32, height: u32, kind: ImageKind) -> Result<usize, Fault> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(limit("image side length", MAX_SIDE as usize));
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .filter(|pixels| *pixels <= kind.pixel_limit())
        .ok_or_else(|| limit("image pixels", kind.pixel_limit()))?;
    pixels
        .checked_mul(4)
        .ok_or_else(|| invalid("RGBA8 size overflows"))
}

/// Fully checks pixels/filters, all chunk checksums and the exact zlib stream,
/// retaining only bounded row scratch rather than a decoded image.
pub fn validate_png(bytes: &[u8], kind: ImageKind) -> Result<ImageInfo, Fault> {
    let header = inspect_envelope(bytes, kind)?;
    let _scratch = reserve_payload(header.scratch_bytes())?;
    validate_deflate(bytes, header.filtered_bytes)?;
    let mut reader = decoder(bytes, &header, false)?;
    let mut widths = header.row_widths();
    while let Some(row) = reader.next_row().map_err(png_fault)? {
        let width = widths.next().ok_or_else(|| invalid("surplus PNG row"))?;
        if header.indexed {
            indexed_row(row.data(), width, &header, None)?;
        }
    }
    if widths.next().is_some() {
        return Err(invalid("missing PNG row"));
    }
    reader.finish().map_err(png_fault)?;
    Ok(header.info)
}

/// The caller retains the reservation for the borrowed compressed input.
/// The returned owner retains its RGBA8 reservation; temporary scratch is released.
pub fn decode_png(bytes: &[u8], kind: ImageKind) -> Result<DecodedImage, Fault> {
    let header = inspect_envelope(bytes, kind)?;
    let _scratch = reserve_payload(header.scratch_bytes())?;
    let reservation = reserve_payload(header.info.rgba_bytes)?;
    validate_deflate(bytes, header.filtered_bytes)?;
    let mut reader = decoder(bytes, &header, true)?;
    let (color, depth) = reader.output_color_type();
    if depth != png::BitDepth::Eight && !header.indexed {
        return Err(invalid("PNG cannot be normalized to eight-bit samples"));
    }
    let mut rgba = allocate(header.info.rgba_bytes)?;
    let stride = header.info.width as usize * 4;
    let mut normalized = if color == png::ColorType::Rgba {
        Vec::new()
    } else {
        allocate(stride)?
    };
    let mut y = 0usize;
    let mut widths = header.row_widths();
    while let Some(row) = reader.next_interlaced_row().map_err(png_fault)? {
        let width = widths.next().ok_or_else(|| invalid("surplus PNG row"))?;
        let row_bytes = width * 4;
        let pixels = if color == png::ColorType::Rgba {
            row.data()
        } else {
            if header.indexed {
                indexed_row(
                    row.data(),
                    width,
                    &header,
                    Some(&mut normalized[..row_bytes]),
                )?;
            } else {
                normalize_row(row.data(), color, &mut normalized[..row_bytes])?;
            }
            &normalized[..row_bytes]
        };
        match row.interlace() {
            png::InterlaceInfo::Null(_) => {
                let start = y * stride;
                rgba[start..start + stride].copy_from_slice(pixels);
                y += 1;
            }
            png::InterlaceInfo::Adam7(pass) => {
                png::expand_interlaced_row(&mut rgba, stride, pixels, pass, 32);
            }
        }
    }
    if widths.next().is_some() {
        return Err(invalid("missing PNG row"));
    }
    reader.finish().map_err(png_fault)?;
    DecodedImage::from_reserved_rgba(header.info.width, header.info.height, rgba, reservation)
}

fn normalize_row(source: &[u8], color: png::ColorType, target: &mut [u8]) -> Result<(), Fault> {
    if color == png::ColorType::Indexed {
        return Err(invalid("PNG palette was not expanded"));
    }
    for (pixel, output) in source
        .chunks_exact(color.samples())
        .zip(target.chunks_exact_mut(4))
    {
        match color {
            png::ColorType::Grayscale => {
                output.copy_from_slice(&[pixel[0], pixel[0], pixel[0], 255])
            }
            png::ColorType::GrayscaleAlpha => {
                output.copy_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]])
            }
            png::ColorType::Rgb => output.copy_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            png::ColorType::Rgba => output.copy_from_slice(pixel),
            png::ColorType::Indexed => unreachable!(),
        }
    }
    Ok(())
}

fn indexed_row(
    source: &[u8],
    width: usize,
    header: &Header<'_>,
    mut output: Option<&mut [u8]>,
) -> Result<(), Fault> {
    for x in 0..width {
        let bit = x * header.depth;
        let index = usize::from(
            (source[bit / 8] >> (8 - header.depth - bit % 8)) & ((1u16 << header.depth) - 1) as u8,
        );
        let rgb = header
            .palette
            .get(index * 3..index * 3 + 3)
            .ok_or_else(|| invalid("PNG pixel refers to an absent palette entry"))?;
        if let Some(target) = output.as_deref_mut() {
            target[x * 4..x * 4 + 3].copy_from_slice(rgb);
            target[x * 4 + 3] = header.transparency.get(index).copied().unwrap_or(255);
        }
    }
    Ok(())
}

struct Header<'a> {
    info: ImageInfo,
    raw_row: usize,
    filtered_bytes: u64,
    exif_bytes: usize,
    passes: [(u32, u32); 7],
    indexed: bool,
    depth: usize,
    palette: &'a [u8],
    transparency: &'a [u8],
}

impl Header<'_> {
    fn row_widths(&self) -> impl Iterator<Item = usize> {
        self.passes
            .into_iter()
            .flat_map(|(width, height)| std::iter::repeat_n(width as usize, height as usize))
    }
    fn scratch_bytes(&self) -> usize {
        // png 0.18.1's Limits excludes its unfilter ring, inflater and some
        // metadata clones. The ring shifts after max(128KiB, align64(4*row));
        // its Vec can double, with a 32KiB lookback and 8KiB refill. Reserve
        // those buffers, both row conversions, inflater state and eXIf clones.
        // Text/ICC decompression is disabled. This is a reservation, not an allocation.
        1024 * 1024 + 32 * self.raw_row + 3 * self.exif_bytes.next_power_of_two()
    }
}

fn inspect_envelope(bytes: &[u8], kind: ImageKind) -> Result<Header<'_>, Fault> {
    if bytes.len() > kind.compressed_limit() {
        return Err(limit("compressed PNG bytes", kind.compressed_limit()));
    }
    if bytes.len() < 33 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[8..16] != b"\0\0\0\rIHDR" {
        return Err(invalid("PNG header is missing or invalid"));
    }
    let width = be_u32(&bytes[16..20]);
    let height = be_u32(&bytes[20..24]);
    let rgba_bytes = checked_rgba_bytes(width, height, kind)?;
    let depth = usize::from(bytes[24]);
    let channels = match (bytes[25], depth) {
        (0, 1 | 2 | 4 | 8 | 16) | (3, 1 | 2 | 4 | 8) => 1,
        (2, 8 | 16) => 3,
        (4, 8 | 16) => 2,
        (6, 8 | 16) => 4,
        _ => return Err(invalid("PNG sample format is unsupported")),
    };
    if bytes[26] != 0 || bytes[27] != 0 || bytes[28] > 1 {
        return Err(invalid(
            "PNG compression, filtering or interlace method is unsupported",
        ));
    }
    let bits = channels * depth;
    let row = |w: u32| (w as usize * bits).div_ceil(8) + 1;
    let passes = if bytes[28] == 0 {
        [
            (width, height),
            (0, 0),
            (0, 0),
            (0, 0),
            (0, 0),
            (0, 0),
            (0, 0),
        ]
    } else {
        [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ]
        .map(|(x, y, dx, dy)| {
            let w = width.saturating_sub(x).div_ceil(dx);
            let h = height.saturating_sub(y).div_ceil(dy);
            (w, if w == 0 { 0 } else { h })
        })
    };
    let filtered_bytes = passes
        .iter()
        .map(|&(w, h)| row(w) as u64 * u64::from(h))
        .sum();
    let mut offset = 8usize;
    let mut idat = false;
    let mut after_idat = false;
    let mut ended = false;
    let mut exif_bytes = 0;
    let mut palette = None;
    let mut transparency = None;
    while offset < bytes.len() {
        let (name, data, next) = chunk(bytes, offset)?;
        if !name.iter().all(u8::is_ascii_alphabetic) || !name[2].is_ascii_uppercase() {
            return Err(invalid("invalid PNG chunk name"));
        }
        if matches!(name, b"acTL" | b"fcTL" | b"fdAT") {
            return Err(invalid("animated PNG is unsupported"));
        }
        match name {
            b"IHDR" if offset != 8 => return Err(invalid("duplicate PNG header")),
            b"PLTE" => {
                if idat
                    || palette.is_some()
                    || transparency.is_some()
                    || matches!(bytes[25], 0 | 4)
                    || data.is_empty()
                    || data.len() > 768
                    || data.len() % 3 != 0
                    || (bytes[25] == 3 && data.len() / 3 > 1usize << depth)
                {
                    return Err(invalid("invalid PNG palette"));
                }
                palette = Some(data);
            }
            b"tRNS" => {
                let valid = match bytes[25] {
                    0 => {
                        data.len() == 2
                            && u32::from(u16::from_be_bytes([data[0], data[1]])) < 1u32 << depth
                    }
                    2 => {
                        data.len() == 6
                            && data.chunks_exact(2).all(|sample| {
                                u32::from(u16::from_be_bytes([sample[0], sample[1]]))
                                    < 1u32 << depth
                            })
                    }
                    3 => palette.is_some_and(|palette: &[u8]| {
                        !data.is_empty() && data.len() <= palette.len() / 3
                    }),
                    _ => false,
                };
                if idat || transparency.is_some() || !valid {
                    return Err(invalid("invalid PNG transparency"));
                }
                transparency = Some(data);
            }
            b"IDAT" => {
                if after_idat {
                    return Err(invalid("PNG image data is not contiguous"));
                }
                if bytes[25] == 3 && palette.is_none() {
                    return Err(invalid("indexed PNG has no palette"));
                }
                idat = true;
            }
            b"IEND" => {
                if !data.is_empty() || !idat || next != bytes.len() {
                    return Err(invalid("PNG terminator is invalid or has trailing data"));
                }
                ended = true;
            }
            b"eXIf" => exif_bytes = exif_bytes.max(data.len()),
            _ => {}
        }
        if idat && name != b"IDAT" {
            after_idat = true;
        }
        offset = next;
    }
    if !ended {
        return Err(invalid("PNG terminator is missing"));
    }
    Ok(Header {
        info: ImageInfo {
            width,
            height,
            rgba_bytes,
        },
        raw_row: row(width),
        filtered_bytes,
        exif_bytes,
        passes,
        indexed: bytes[25] == 3,
        depth,
        palette: palette.unwrap_or(&[]),
        transparency: transparency.unwrap_or(&[]),
    })
}

fn chunk(bytes: &[u8], offset: usize) -> Result<(&[u8], &[u8], usize), Fault> {
    let prefix = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| invalid("truncated PNG chunk"))?;
    let length = be_u32(&prefix[..4]) as usize;
    let next = offset
        .checked_add(12)
        .and_then(|value| value.checked_add(length))
        .filter(|next| *next <= bytes.len())
        .ok_or_else(|| invalid("truncated PNG chunk"))?;
    Ok((&prefix[4..], &bytes[offset + 8..next - 4], next))
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn decoder<'a>(
    bytes: &'a [u8],
    header: &Header<'_>,
    normalize: bool,
) -> Result<png::Reader<Cursor<&'a [u8]>>, Fault> {
    let mut options = png::DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    options.set_ignore_text_chunk(true);
    options.set_ignore_iccp_chunk(true);
    let mut decoder = png::Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_limits(png::Limits {
        bytes: header.scratch_bytes(),
    });
    if normalize && !header.indexed {
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    }
    decoder.read_info().map_err(png_fault)
}

fn validate_deflate(bytes: &[u8], expected: u64) -> Result<(), Fault> {
    // png deliberately accepts missing zlib tails and surplus compressed data.
    // An independent fixed-buffer pass closes that acceptance gap without ever
    // allocating the expanded stream or collecting the IDAT chunks.
    let mut inflater = Decompress::new(true);
    let mut output = [0u8; 8192];
    let mut ended = false;
    let mut offset = 8;
    while offset < bytes.len() {
        let (name, mut data, next) = chunk(bytes, offset)?;
        offset = next;
        if name != b"IDAT" {
            continue;
        }
        loop {
            if ended {
                if !data.is_empty() {
                    return Err(invalid("surplus PNG compressed image data"));
                }
                break;
            }
            let before_in = inflater.total_in();
            let before_out = inflater.total_out();
            let remaining = expected.saturating_sub(before_out);
            let length = output.len().min(
                usize::try_from(remaining)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1),
            );
            let status = inflater
                .decompress(data, &mut output[..length], FlushDecompress::None)
                .map_err(|_| invalid("invalid PNG zlib stream or checksum"))?;
            let consumed = (inflater.total_in() - before_in) as usize;
            data = &data[consumed..];
            if inflater.total_out() > expected {
                return Err(invalid("PNG expands beyond its declared scanlines"));
            }
            if status == Status::StreamEnd {
                ended = true;
                continue;
            }
            if consumed == 0 && inflater.total_out() == before_out {
                if !data.is_empty() {
                    return Err(invalid("invalid PNG compressed image data"));
                }
                break;
            }
        }
    }
    if !ended || inflater.total_out() != expected {
        return Err(invalid("truncated PNG image data or zlib checksum"));
    }
    Ok(())
}

/// Encodes original pixels row by row, with no full crop copy or inherited metadata.
pub fn encode_crop(image: &DecodedImage, rect: [u32; 4]) -> Result<EncodedImage, Fault> {
    image.validate()?;
    let [x, y, width, height] = rect;
    checked_rgba_bytes(width, height, ImageKind::Crop)?;
    if x.checked_add(width).is_none_or(|right| right > image.width)
        || y.checked_add(height)
            .is_none_or(|bottom| bottom > image.height)
    {
        return Err(invalid("crop lies outside the original image"));
    }
    // The streaming flate2/miniz encoder retains bounded compressor state,
    // three PNG scanlines and the chosen 8KiB output chunk, not a crop raster.
    // Avoid fdeflate's fast writer: its final flush unwraps downstream errors.
    let _scratch = reserve_payload(width as usize * 4 * 3 + 1024 * 1024)?;
    let mut output = EncodedImage {
        bytes: Vec::new(),
        reservation: reserve_payload(0)?,
    };
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Balanced);
        let mut writer = encoder.write_header().map_err(png_fault)?;
        {
            let mut stream = writer.stream_writer_with_size(8192).map_err(png_fault)?;
            let stride = image.width as usize * 4;
            for row in y..y + height {
                let start = row as usize * stride + x as usize * 4;
                stream
                    .write_all(&image.rgba[start..start + width as usize * 4])
                    .map_err(png_fault)?;
            }
            stream.finish().map_err(png_fault)?;
        }
        writer.finish().map_err(png_fault)?;
    }
    Ok(output)
}

impl Write for EncodedImage {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let length = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|length| *length <= CROP_MAX_BYTES)
            .ok_or_else(|| std::io::Error::other("compressed crop byte limit exceeded"))?;
        grow_payload(
            &mut self.bytes,
            &mut self.reservation,
            length,
            CROP_MAX_BYTES,
        )
        .map_err(std::io::Error::other)?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Display-only nearest-neighbor preview. It never modifies the original frame.
pub fn preview(image: &DecodedImage) -> Result<DecodedImage, Fault> {
    image.validate()?;
    let pixels = u64::from(image.width) * u64::from(image.height);
    let (width, height) = if pixels <= CROP_MAX_PIXELS as u64 {
        (image.width, image.height)
    } else {
        let scale = (CROP_MAX_PIXELS as f64 / pixels as f64).sqrt();
        (
            (f64::from(image.width) * scale).floor().max(1.0) as u32,
            (f64::from(image.height) * scale).floor().max(1.0) as u32,
        )
    };
    let length = checked_rgba_bytes(width, height, ImageKind::Crop)?;
    let reservation = reserve_payload(length)?;
    let mut rgba = allocate(length)?;
    for y in 0..height {
        let source_y = u64::from(y) * u64::from(image.height) / u64::from(height);
        for x in 0..width {
            let source_x = u64::from(x) * u64::from(image.width) / u64::from(width);
            let source = (source_y * u64::from(image.width) + source_x) as usize * 4;
            let target = (y as usize * width as usize + x as usize) * 4;
            rgba[target..target + 4].copy_from_slice(&image.rgba[source..source + 4]);
        }
    }
    DecodedImage::from_reserved_rgba(width, height, rgba, reservation)
}

fn allocate(bytes: usize) -> Result<Vec<u8>, Fault> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(bytes)
        .map_err(|_| limit("image allocation bytes", bytes))?;
    output.resize(bytes, 0);
    Ok(output)
}

fn invalid(message: &str) -> Fault {
    Fault::new("InvalidImage", message)
}
fn limit(category: &str, ceiling: usize) -> Fault {
    Fault::new("ImageLimit", format!("{category} limit exceeded"))
        .with_context(json!({"resource":category,"limit":ceiling}))
}
fn png_fault(error: impl std::fmt::Display) -> Fault {
    Fault::new(
        "InvalidImage",
        format!("PNG validation or encoding failed: {error}"),
    )
}

#[cfg(test)]
mod tests;
