//! Image scaling for the Swiss Postcard Creator (port of `ImageHelper`).
//!
//! PCC requires 1819x1311. The .NET code (ImageMagick):
//! * rotate 90° if the image is taller than wide
//! * scale with `1819x1311^` + fill (max-fit, aspect preserved)
//! * center-crop to exactly 1819x1311
//! * encode JPEG and return base64
//!
//! `image::DynamicImage::resize_exact` is the direct equivalent of the
//! "scale-to-fill + center-crop" pipeline.
use base64::Engine;

/// Forced width.
const TARGET_WIDTH: u32 = 1819;
/// Forced height.
const TARGET_HEIGHT: u32 = 1311;

/// Scale an image to be compatible with the Swiss Postcard Creator.
pub fn scale_and_convert_to_base64(data: &[u8]) -> anyhow::Result<String> {
    let img = match image::load_from_memory(data) {
        Ok(img) => img,
        Err(orig_err) => {
            if is_heif_data(data) {
                if let Ok(jpeg_bytes) = try_convert_heif_to_jpeg(data) {
                    image::load_from_memory(&jpeg_bytes)?
                } else {
                    return Err(orig_err.into());
                }
            } else {
                return Err(orig_err.into());
            }
        }
    };
    let mut img = if img.width() < img.height() {
        img.rotate90()
    } else {
        img
    };

    // Fill 1819x1311 preserving aspect, then center-crop (same as `^` + Crop).
    img = img.resize_exact(
        TARGET_WIDTH,
        TARGET_HEIGHT,
        image::imageops::FilterType::Lanczos3,
    );

    // JPEG quality 92 = ImageMagick default.
    let mut buf: Vec<u8> = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 92);
    img.write_with_encoder(encoder)?;

    Ok(base64::engine::general_purpose::STANDARD.encode(buf))
}

/// Check if raw data starts with ISO-BMFF / HEIF signatures.
pub fn is_heif_data(data: &[u8]) -> bool {
    if data.len() < 12 {
        return false;
    }
    if &data[4..8] != b"ftyp" {
        return false;
    }
    let brand = &data[8..12];
    matches!(
        brand,
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1"
    )
}

/// Attempt converting HEIF/HEIC data to JPEG using system tools (heif-convert or magick) if available.
fn try_convert_heif_to_jpeg(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let temp_dir = std::env::temp_dir();
    let id = rand::random::<u64>();
    let input_path = temp_dir.join(format!("pcd_input_{id}.heic"));
    let output_path = temp_dir.join(format!("pcd_output_{id}.jpg"));

    std::fs::write(&input_path, data)?;

    // 1. Try `heif-convert input.heic output.jpg`
    let res = std::process::Command::new("heif-convert")
        .arg(&input_path)
        .arg(&output_path)
        .output();

    let converted = match res {
        Ok(out) if out.status.success() && output_path.exists() => {
            std::fs::read(&output_path)
        }
        _ => {
            // 2. Try `magick input.heic output.jpg` or `convert input.heic output.jpg`
            let magick_res = std::process::Command::new("magick")
                .arg(&input_path)
                .arg(&output_path)
                .output()
                .or_else(|_| {
                    std::process::Command::new("convert")
                        .arg(&input_path)
                        .arg(&output_path)
                        .output()
                });

            match magick_res {
                Ok(out) if out.status.success() && output_path.exists() => {
                    std::fs::read(&output_path)
                }
                _ => {
                    let _ = std::fs::remove_file(&input_path);
                    let _ = std::fs::remove_file(&output_path);
                    anyhow::bail!("No system HEIC converter available (heif-convert / magick)");
                }
            }
        }
    };

    let _ = std::fs::remove_file(&input_path);
    let _ = std::fs::remove_file(&output_path);

    converted.map_err(|e| anyhow::anyhow!("Failed reading converted HEIC image: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(img: image::DynamicImage) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        img.write_to(&mut cursor, image::ImageFormat::Jpeg).unwrap();
        cursor.into_inner()
    }

    #[test]
    fn scales_landscape() {
        // 4x3 landscape image
        let img = image::DynamicImage::new(TARGET_WIDTH * 2, TARGET_HEIGHT * 2, image::ColorType::Rgb8);
        let buf = encode(img);
        let out = scale_and_convert_to_base64(&buf).unwrap();
        assert!(!out.is_empty());
        let bytes = base64::engine::general_purpose::STANDARD.decode(&out).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (TARGET_WIDTH, TARGET_HEIGHT));
    }

    #[test]
    fn rotates_portrait() {
        // Portrait: width < height -> should be rotated first
        let img = image::DynamicImage::new(100, 200, image::ColorType::Rgb8);
        let buf = encode(img);
        let out = scale_and_convert_to_base64(&buf).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(&out).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (TARGET_WIDTH, TARGET_HEIGHT));
    }
}
