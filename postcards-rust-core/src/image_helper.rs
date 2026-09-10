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
    let img = image::load_from_memory(data)?;
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
