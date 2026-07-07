//! Remote AI masking via the rr-ai-gateway (see docs/superpowers/specs/
//! 2026-07-07-remote-ai-masking-design.md). Upload once (content-addressed),
//! enqueue a mask job, poll, return parameters for a `remote-ai` sub-mask.

use std::io::Cursor;

use image::codecs::tiff::TiffEncoder;
use image::{DynamicImage, ExtendedColorType, ImageEncoder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Clone, Debug)]
pub struct MaskJobParams {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<[f64; 3]>>,
    // Every Option MUST be skipped when None: the gateway validates params with
    // JSON Schema and `"roi_mask_b64": null` fails `{"type": "string"}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roi_mask_b64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agentic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct GatewayMaskResult {
    pub mask_png_b64: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default = "default_alignment")]
    pub alignment: String,
    #[serde(default)]
    pub timings: Value,
}

fn default_alignment() -> String {
    "exact".to_string()
}

/// Encode an image as a 16-bit linear RGB TIFF for lossless upload to the gateway.
pub fn encode_linear_tiff(img: &DynamicImage) -> Result<Vec<u8>, String> {
    let rgb16 = img.to_rgb16();
    let (w, h) = (rgb16.width(), rgb16.height());
    let mut buf = Cursor::new(Vec::new());
    let encoder = TiffEncoder::new(&mut buf);
    let raw: &[u16] = rgb16.as_raw();
    let bytes: Vec<u8> = raw.iter().flat_map(|v| v.to_le_bytes()).collect();
    encoder
        .write_image(&bytes, w, h, ExtendedColorType::Rgb16)
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbaImage};

    #[test]
    fn tiff_export_roundtrip() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_fn(8, 6, |x, y| {
            image::Rgba([x as u8 * 10, y as u8 * 10, 128, 255])
        }));
        let bytes = encode_linear_tiff(&img).unwrap();
        let back = image::load_from_memory(&bytes).unwrap();
        assert_eq!((back.width(), back.height()), (8, 6));
        assert!(matches!(back.color(), image::ColorType::Rgb16));
    }

    #[test]
    fn mask_request_serializes_snake_json() {
        let req = MaskJobParams {
            mode: "prompt".into(),
            query: Some("the dog.".into()),
            points: None,
            roi_mask_b64: None,
            preset: None,
            agentic: Some(false),
            backend: Some("sam2".into()),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["mode"], "prompt");
        assert_eq!(v["query"], "the dog.");
        // None options must be ABSENT (gateway schema rejects nulls)
        assert!(v.get("points").is_none());
        assert!(v.get("roi_mask_b64").is_none());
        assert!(v.get("preset").is_none());
    }

    #[test]
    fn result_params_deserialize_from_gateway_result() {
        let json = serde_json::json!({
            "mask_png_b64": "aGk=", "width": 100, "height": 60,
            "labels": ["person"], "alignment": "exact",
            "timings": {"total_s": 1.5}
        });
        let r: GatewayMaskResult = serde_json::from_value(json).unwrap();
        assert_eq!(r.width, 100);
        assert_eq!(r.alignment, "exact");
        assert_eq!(r.labels, vec!["person"]);
    }
}
