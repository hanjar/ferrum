//! Quick View image downsampling (FER-103). A big image is never handed to the
//! webview whole: the header is read first (dimensions only), hostile or
//! absurd headers are refused BEFORE any pixel buffer is allocated, and
//! anything over the passthrough thresholds is decoded here under
//! `image::Limits`, shrunk to at most [`MAX_OUTPUT_DIM`] px on the long side,
//! re-encoded (PNG when it has alpha, JPEG otherwise) and returned as base64.
//! Small images stay on the `ferrum-media` path (`downsampled = false`, no
//! bytes shipped).

use crate::domain::{ErrorCode, FsError, validate_path};
use crate::preview::image_mime_by_extension;
use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;

/// Images up to this size on both sides (and under the byte threshold) are
/// served straight from disk by the webview.
pub const PASSTHROUGH_MAX_DIM: u32 = 4096;
/// Byte threshold for the same passthrough decision.
pub const PASSTHROUGH_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Longest side of a downsampled result.
pub const MAX_OUTPUT_DIM: u32 = 2048;
/// Decompression-bomb guard: pixel count taken from the header.
pub const MAX_SOURCE_PIXELS: u64 = 100_000_000;
/// Source files larger than this are refused outright.
pub const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
/// Decoder allocation ceiling handed to `image::Limits`.
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;
/// Budget for the decoded pixel buffer (`decoder.total_bytes()`), checked
/// BEFORE decoding: image 0.25 `from_decoder` does not reserve it against
/// `Limits`, so a tiny, highly compressed 16-bit PNG could otherwise expand
/// past the intended 512 MiB peak. The downscale (`thumbnail_exact`) only
/// allocates the small output, so peak stays near this figure.
pub const MAX_DECODED_BYTES: u64 = 384 * 1024 * 1024;
/// Largest encoded result sent over IPC (base64 adds a third on top).
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const JPEG_QUALITY: u8 = 85;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagePreview {
    /// True when `data` carries a re-encoded, shrunk copy; false means the
    /// webview should load the original through the `ferrum-media` protocol
    /// (unless `inline`).
    pub downsampled: bool,
    /// True when `data` holds the ORIGINAL bytes (SVG, which the media
    /// protocol never serves); shown as a `data:` URL in an inert <img>.
    #[serde(default)]
    pub inline: bool,
    /// MIME of `data` (downsampled) or of the original (passthrough).
    pub mime: String,
    /// Base64 of the encoded copy; empty when `downsampled` is false.
    pub data: String,
    /// Dimensions of what `data` shows (equal to the original when passthrough).
    pub width: u32,
    pub height: u32,
    /// Dimensions of the original file (0 when the format has no raster header, e.g. SVG).
    pub original_width: u32,
    pub original_height: u32,
}

/// Largest SVG shipped inline.
const SVG_INLINE_MAX_BYTES: u64 = 2 * 1024 * 1024;

fn read_svg_inline(file: File, path: &str, size: u64) -> Result<ImagePreview, FsError> {
    use base64::Engine;
    use std::io::Read;
    if size > SVG_INLINE_MAX_BYTES {
        return Err(unsupported("SVG file too large to preview"));
    }
    let mut bytes = Vec::new();
    file.take(SVG_INLINE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| FsError::from(e).with_path(path))?;
    if bytes.len() as u64 > SVG_INLINE_MAX_BYTES {
        return Err(unsupported("SVG file too large to preview"));
    }
    Ok(ImagePreview {
        downsampled: false,
        inline: true,
        mime: "image/svg+xml".to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        width: 0,
        height: 0,
        original_width: 0,
        original_height: 0,
    })
}

fn unsupported(msg: &str) -> FsError {
    FsError::new(ErrorCode::Unsupported, msg)
}

fn passthrough(mime: &str, w: u32, h: u32) -> ImagePreview {
    ImagePreview {
        downsampled: false,
        inline: false,
        mime: mime.to_string(),
        data: String::new(),
        width: w,
        height: h,
        original_width: w,
        original_height: h,
    }
}

/// Entry point used by the IPC command.
pub fn read_image_preview(path: &str) -> Result<ImagePreview, FsError> {
    preview_with(path, PASSTHROUGH_MAX_BYTES)
}

pub(crate) fn preview_with(path: &str, passthrough_bytes: u64) -> Result<ImagePreview, FsError> {
    validate_path(path)?;
    // FER-486 A1/B1: same read-path guard as Quick View; metadata and reads use the checked
    // handle (the symlink TARGET for a link), never the path again.
    let guarded = match crate::path_guard::open_entry(path)? {
        crate::path_guard::Entry::File(file) => file,
        _ => {
            return Err(FsError::new(
                ErrorCode::NotADirectory,
                "Cannot preview a folder — select a file",
            )
            .with_path(path));
        }
    };
    let path = guarded.path.as_str();
    let p = Path::new(path);
    // Same cloud-placeholder rule as read_preview: never hydrate from a preview.
    if guarded.offline {
        return Err(crate::preview::offline_error().with_path(path));
    }
    let size = guarded.meta.len();
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(ext_mime) = image_mime_by_extension(&name) else {
        return Err(unsupported("Not an image file"));
    };
    // SVG is never served by `ferrum-media` (it can carry script): its bytes
    // travel inline and an <img> renders them inertly from a data: URL.
    if ext_mime == "image/svg+xml" {
        return read_svg_inline(guarded.open_read()?, path, size);
    }
    // ICO has no raster header the decoder set understands; it is plain image
    // data served by the media protocol.
    if ext_mime == "image/x-icon" {
        return Ok(passthrough(ext_mime, 0, 0));
    }
    if size > MAX_SOURCE_BYTES {
        return Err(unsupported("Image file too large to preview"));
    }

    // 1) header only: format + dimensions, no pixel buffer yet.
    let (mut decoder, format) = open_decoder(guarded.open_read()?, path)?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
        return Err(unsupported(
            "Image dimensions are too large to preview safely",
        ));
    }
    if w <= PASSTHROUGH_MAX_DIM && h <= PASSTHROUGH_MAX_DIM && size <= passthrough_bytes {
        return Ok(passthrough(ext_mime, w, h));
    }

    let total = decoder.total_bytes();
    if total > MAX_DECODED_BYTES {
        return Err(unsupported("Image is too large to preview safely"));
    }
    // zune-jpeg keeps i16 coefficients for the WHOLE image (progressive in
    // particular) and image does not pass max_alloc to it: budget those 8
    // bytes per pixel on top of the output buffer.
    if format == ImageFormat::Jpeg
        && !jpeg_within_budget(w, h, decoder.color_type().bytes_per_pixel())
    {
        return Err(unsupported("Image is too large to preview safely"));
    }

    // 2) decode under hard limits, shrink, re-encode.
    let mut limits = Limits::default();
    // GIF reads the frame under max_alloc: leave room for the output buffer.
    limits.max_alloc = Some(MAX_DECODE_ALLOC.saturating_sub(total));
    decoder
        .set_limits(limits)
        .map_err(|_| unsupported("Image is too large to preview safely"))?;
    let img = DynamicImage::from_decoder(decoder)
        .map_err(|_| unsupported("Image could not be decoded (damaged or unsupported)"))?;
    let has_alpha = img.color().has_alpha();
    let mut dim = MAX_OUTPUT_DIM;
    let (small, bytes, mime) = loop {
        let small = shrink(&img, dim);
        let (bytes, mime) = encode(&small, has_alpha)?;
        if bytes.len() <= MAX_OUTPUT_BYTES {
            break (small, bytes, mime);
        }
        // Too big for IPC (noisy alpha PNG): retry smaller, then give up.
        dim /= 2;
        if dim < 256 {
            return Err(unsupported("Image preview result is too large"));
        }
    };
    let (sw, sh) = (small.width(), small.height());
    Ok(ImagePreview {
        downsampled: true,
        inline: false,
        mime: mime.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        width: sw,
        height: sh,
        original_width: w,
        original_height: h,
    })
}

/// Open the file and build a decoder for a supported raster format only.
fn open_decoder(
    file: File,
    path: &str,
) -> Result<(Box<dyn ImageDecoder + 'static>, ImageFormat), FsError> {
    let reader = ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .map_err(|e| FsError::from(e).with_path(path))?;
    let format = match reader.format() {
        Some(
            f @ (ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Gif
            | ImageFormat::WebP
            | ImageFormat::Bmp),
        ) => f,
        _ => return Err(unsupported("Image format not supported for preview")),
    };
    let decoder = reader
        .into_decoder()
        .map_err(|_| unsupported("Image could not be read (damaged or unsupported)"))?;
    Ok((Box::new(decoder), format))
}

/// `pixels * (output bytes per pixel + 8)` must fit the decode allocation budget.
pub(crate) fn jpeg_within_budget(w: u32, h: u32, out_bpp: u8) -> bool {
    u64::from(w) * u64::from(h) * (u64::from(out_bpp) + 8) <= MAX_DECODE_ALLOC
}

fn shrink(img: &DynamicImage, max_dim: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    let long = w.max(h);
    if long <= max_dim {
        return img.clone();
    }
    let scale = f64::from(max_dim) / f64::from(long);
    let nw = ((f64::from(w) * scale).round() as u32).max(1);
    let nh = ((f64::from(h) * scale).round() as u32).max(1);
    img.thumbnail_exact(nw, nh)
}

fn encode(img: &DynamicImage, has_alpha: bool) -> Result<(Vec<u8>, &'static str), FsError> {
    let mut out: Vec<u8> = Vec::new();
    let fail = |_| FsError::new(ErrorCode::Io, "Could not encode the image preview");
    if has_alpha {
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .map_err(fail)?;
        Ok((out, "image/png"))
    } else {
        let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
        rgb.write_with_encoder(JpegEncoder::new_with_quality(
            Cursor::new(&mut out),
            JPEG_QUALITY,
        ))
        .map_err(fail)?;
        Ok((out, "image/jpeg"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ferrum-p5img-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn run(p: &Path) -> Result<ImagePreview, FsError> {
        read_image_preview(p.to_str().unwrap())
    }

    fn png_chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_be_bytes().to_vec();
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(body);
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v.extend_from_slice(&crc.finalize().to_be_bytes());
        v
    }

    /// A PNG whose IHDR claims `w` x `h` but carries no pixel data.
    fn lying_png(w: u32, h: u32, depth: u8, color: u8) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[depth, color, 0, 0, 0]);
        v.extend(png_chunk(b"IHDR", &ihdr));
        v.extend(png_chunk(b"IEND", &[]));
        v
    }

    #[test]
    fn normal_image_is_passthrough_and_ships_no_bytes() {
        let d = dir();
        let p = d.join("small.png");
        RgbImage::from_fn(100, 80, |x, y| Rgb([x as u8, y as u8, 7]))
            .save(&p)
            .unwrap();
        let r = run(&p).unwrap();
        assert!(!r.downsampled);
        assert!(r.data.is_empty());
        assert_eq!((r.width, r.height), (100, 80));
        assert_eq!((r.original_width, r.original_height), (100, 80));
        assert_eq!(r.mime, "image/png");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn oversized_dimensions_are_resized_to_2048() {
        let d = dir();
        let p = d.join("wide.png");
        RgbImage::from_fn(5000, 3000, |x, y| Rgb([(x / 20) as u8, (y / 12) as u8, 90]))
            .save(&p)
            .unwrap();
        let r = run(&p).unwrap();
        assert!(r.downsampled);
        assert_eq!((r.original_width, r.original_height), (5000, 3000));
        assert_eq!(r.width, 2048);
        assert_eq!(r.height, 1229); // 3000 * 2048 / 5000 rounded
        assert_eq!(r.mime, "image/jpeg"); // no alpha
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&r.data)
            .unwrap();
        let back = image::load_from_memory(&raw).unwrap();
        assert_eq!((back.width(), back.height()), (2048, 1229));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn alpha_images_stay_png_and_byte_threshold_also_triggers() {
        let d = dir();
        let p = d.join("alpha.png");
        RgbaImage::from_fn(300, 200, |x, y| Rgba([x as u8, y as u8, 1, 128]))
            .save(&p)
            .unwrap();
        // tiny byte threshold forces the decode path even for a small image
        let r = preview_with(p.to_str().unwrap(), 10).unwrap();
        assert!(r.downsampled);
        assert_eq!(r.mime, "image/png");
        // smaller than the output cap: never upscaled
        assert_eq!((r.width, r.height), (300, 200));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn jpeg_gif_bmp_decode_paths_work() {
        let d = dir();
        for name in ["a.jpg", "b.gif", "c.bmp"] {
            let p = d.join(name);
            RgbImage::from_fn(64, 48, |x, y| Rgb([x as u8 * 3, y as u8 * 4, 9]))
                .save(&p)
                .unwrap();
            let r = preview_with(p.to_str().unwrap(), 1).unwrap();
            assert!(r.downsampled, "{name}");
            assert_eq!((r.width, r.height), (64, 48), "{name}");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn lying_header_with_giant_dimensions_is_refused_before_decoding() {
        let d = dir();
        let p = d.join("bomb.png");
        fs::write(&p, lying_png(60_000, 60_000, 8, 2)).unwrap();
        // the PNG decoder may already refuse this header; either way, no decode
        assert_eq!(run(&p).unwrap_err().code, ErrorCode::Unsupported);
        // BMP header claiming 20000 x 20000 (400 MP): hits OUR pixel cap
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&[0, 0, 0, 0]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&20_000i32.to_le_bytes());
        bmp.extend_from_slice(&20_000i32.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        bmp.extend_from_slice(&[0u8; 24]);
        let pb = d.join("bomb.bmp");
        fs::write(&pb, &bmp).unwrap();
        let e = run(&pb).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        assert!(e.message.contains("too large"), "{}", e.message);
        // within the pixel cap but a 16-bit RGBA allocation far past the ceiling
        let p2 = d.join("bomb2.png");
        fs::write(&p2, lying_png(10_000, 10_000, 16, 6)).unwrap();
        assert!(run(&p2).is_err());
        // huge u32 dimensions must not overflow the pixel maths
        let p3 = d.join("bomb3.png");
        fs::write(&p3, lying_png(u32::MAX >> 1, u32::MAX >> 1, 8, 2)).unwrap();
        assert!(run(&p3).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn damaged_files_are_errors_not_panics() {
        let d = dir();
        let junk = d.join("junk.png");
        fs::write(&junk, b"this is not an image at all").unwrap();
        assert_eq!(run(&junk).unwrap_err().code, ErrorCode::Unsupported);
        // valid header, truncated body, wide enough to need decoding
        let good = d.join("good.png");
        RgbImage::from_fn(4200, 100, |x, y| Rgb([x as u8, y as u8, 3]))
            .save(&good)
            .unwrap();
        let mut bytes = fs::read(&good).unwrap();
        bytes.truncate(bytes.len() / 2);
        let cut = d.join("cut.png");
        fs::write(&cut, &bytes).unwrap();
        assert_eq!(run(&cut).unwrap_err().code, ErrorCode::Unsupported);
        // empty file
        let empty = d.join("empty.jpg");
        fs::write(&empty, b"").unwrap();
        assert!(run(&empty).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn svg_ico_missing_dir_and_non_images_are_handled() {
        let d = dir();
        let svg = d.join("x.svg");
        fs::write(&svg, "<svg/>").unwrap();
        let r = run(&svg).unwrap();
        assert!(!r.downsampled && r.inline && r.mime == "image/svg+xml");
        assert!(!r.data.is_empty(), "SVG bytes travel inline");
        assert_eq!(
            run(&d.join("nope.png")).unwrap_err().code,
            ErrorCode::NotFound
        );
        assert_eq!(run(&d).unwrap_err().code, ErrorCode::NotADirectory);
        let txt = d.join("a.txt");
        fs::write(&txt, "hi").unwrap();
        assert_eq!(run(&txt).unwrap_err().code, ErrorCode::Unsupported);
        // content in a format the build does not decode (TIFF header)
        let tif = d.join("t.png");
        fs::write(&tif, [b'I', b'I', b'*', 0, 8, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(run(&tif).unwrap_err().code, ErrorCode::Unsupported);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn decoded_buffer_budget_rejects_a_compressed_16bit_bomb() {
        // 10000 x 10000 RGBA16 = 800 MB decoded, solid colour => a few hundred KB on disk
        let d = dir();
        let p = d.join("bomb16.png");
        let f = fs::File::create(&p).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(f), 10_000, 10_000);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Sixteen);
        enc.set_compression(png::Compression::Fast);
        let mut w = enc.write_header().unwrap().into_stream_writer().unwrap();
        let row = vec![0x7fu8; 10_000 * 8];
        for _ in 0..10_000 {
            std::io::Write::write_all(&mut w, &row).unwrap();
        }
        w.finish().unwrap();
        assert!(fs::metadata(&p).unwrap().len() < 64 * 1024 * 1024);
        let e = run(&p).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        assert!(e.message.contains("too large"), "{}", e.message);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn big_rgba_within_budget_decodes_and_stays_under_the_output_cap() {
        let d = dir();
        let p = d.join("rgba.png");
        // 5000 x 4000 RGBA8 = 80 MB decoded: inside the budget, needs downsampling
        RgbaImage::from_fn(5000, 4000, |x, y| {
            Rgba([(x / 20) as u8, (y / 16) as u8, 5, 200])
        })
        .save(&p)
        .unwrap();
        let r = run(&p).unwrap();
        assert!(r.downsampled);
        assert_eq!(r.mime, "image/png");
        assert_eq!(r.width, 2048);
        assert!(r.data.len() / 4 * 3 <= MAX_OUTPUT_BYTES);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn jpeg_header_claiming_a_huge_image_is_refused_before_decoding() {
        let d = dir();
        let p = d.join("big.jpg");
        RgbImage::from_fn(64, 48, |x, y| Rgb([x as u8, y as u8, 9]))
            .save(&p)
            .unwrap();
        let mut b = fs::read(&p).unwrap();
        // patch the SOF marker (FFC0 baseline / FFC2 progressive): len(2) prec(1) height(2) width(2)
        let i = b
            .windows(2)
            .position(|w| w == [0xFF, 0xC0] || w == [0xFF, 0xC2])
            .unwrap();
        b[i + 5..i + 7].copy_from_slice(&8000u16.to_be_bytes());
        b[i + 7..i + 9].copy_from_slice(&8000u16.to_be_bytes());
        fs::write(&p, &b).unwrap(); // 64 MP, tiny file
        let e = run(&p).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        assert!(e.message.contains("too large"), "{}", e.message);
        assert!(jpeg_within_budget(6900, 6900, 3)); // ~47.6 MP RGB
        assert!(!jpeg_within_budget(7000, 7000, 3));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn preview_serializes_camel_case() {
        let v = serde_json::to_value(passthrough("image/png", 3, 2)).unwrap();
        assert_eq!(v["downsampled"], false);
        assert_eq!(v["originalWidth"], 3);
        assert_eq!(v["mime"], "image/png");
    }
}
