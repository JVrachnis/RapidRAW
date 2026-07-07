//! Remote AI masking via the rr-ai-gateway (see docs/superpowers/specs/
//! 2026-07-07-remote-ai-masking-design.md). Upload once (content-addressed),
//! enqueue a mask job, poll, return parameters for a `remote-ai` sub-mask.

use std::io::Cursor;
use std::path::Path;

use image::codecs::tiff::TiffEncoder;
use image::{DynamicImage, ExtendedColorType, ImageEncoder};
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::Emitter;

use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::get_cached_full_warped_image;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sam3_multirep: Option<bool>,
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
    let bytes: Vec<u8> = raw.iter().flat_map(|v| v.to_ne_bytes()).collect();
    encoder
        .write_image(&bytes, w, h, ExtendedColorType::Rgb16)
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

/// Normalize a configured gateway/connector address into a full base URL:
/// bare hosts (`host:port`) get an `http://` scheme prepended; addresses that
/// already specify `http://` or `https://` are passed through unchanged.
fn normalize_base_address(addr: &str) -> String {
    if addr.starts_with("http://") || addr.starts_with("https://") {
        addr.to_string()
    } else {
        format!("http://{}", addr)
    }
}

#[derive(Deserialize, Debug)]
struct SourceResponse {
    source_id: String,
}

#[derive(Deserialize, Debug)]
struct JobSubmitted {
    job_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    queue_position: Option<u32>,
}

#[derive(Deserialize, Debug)]
struct JobStatus {
    status: String,
    #[serde(default)]
    queue_position: Option<u32>,
    #[serde(default)]
    progress: Option<f64>,
    #[serde(default)]
    result: Option<GatewayMaskResult>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Serialize, Clone)]
struct MaskStatusEvent {
    sub_mask_id: String,
    stage: String, // "uploading" | "queued" | "running" | "done" | "error"
    queue_position: Option<u32>,
    progress: Option<f64>,
    detail: Option<String>,
}

fn emit_status(app: &tauri::AppHandle, ev: MaskStatusEvent) {
    let _ = app.emit("remote-mask-status", ev);
}

/// Resolve the gateway base URL and optional bearer token from settings.
/// Prefers `remote_mask_address`, falling back to `ai_connector_address`.
fn gateway_base(app_handle: &tauri::AppHandle) -> Result<(String, Option<String>), String> {
    let settings = load_settings(app_handle.clone())?;
    let addr = settings
        .remote_mask_address
        .filter(|s| !s.is_empty())
        .or(settings.ai_connector_address.clone())
        .ok_or("No AI backend address configured")?;
    let base = normalize_base_address(&addr);
    // No separate bearer-token setting exists yet on AppSettings; mirror
    // ai_connector.rs's currently tokenless local-network usage.
    Ok((base, None))
}

#[allow(clippy::too_many_arguments)]
async fn upload_source(
    client: &reqwest::Client,
    base: &str,
    token: Option<&str>,
    bytes: Vec<u8>,
    filename: &str,
    exif_json: Option<String>,
    rrdata_json: Option<String>,
    dims: Option<(u32, u32)>,
) -> Result<String, String> {
    let part = multipart::Part::bytes(bytes)
        .file_name(filename.to_string())
        .mime_str("application/octet-stream")
        .map_err(|e| e.to_string())?;
    let mut form = multipart::Form::new().part("file", part);
    if let Some(e) = exif_json {
        form = form.text("exif", e);
    }
    if let Some(r) = rrdata_json {
        form = form.text("rrdata", r);
    }
    if let Some((w, h)) = dims {
        form = form.text("client_width", w.to_string());
        form = form.text("client_height", h.to_string());
    }
    let mut req = client.post(format!("{}/sources", base)).multipart(form);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("source upload failed: HTTP {}", resp.status()));
    }
    let sr: SourceResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(sr.source_id)
}

fn gather_exif(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let map = crate::exif_processing::read_exif_data(path, &bytes);
    serde_json::to_string(&map).ok()
}

fn gather_rrdata(path: &str) -> Option<String> {
    let sidecar = crate::exif_processing::get_primary_sidecar_path(Path::new(path));
    std::fs::read_to_string(sidecar).ok()
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMaskRequest {
    pub sub_mask_id: String,
    pub path: String,
    pub mode: String,
    pub query: Option<String>,
    pub points: Option<Vec<[f64; 3]>>,
    pub roi_mask_b64: Option<String>,
    pub preset: Option<String>,
    pub agentic: Option<bool>,
    pub sam3_multirep: Option<bool>,
    pub rotation: f32,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub orientation_steps: u8,
    pub js_adjustments: Value,
}

/// Encode the upload payload (TIFF-encode + hash) off the async executor:
/// with a 150-300MB warped-image buffer, doing this inline would stall every
/// other task on the Tokio runtime.
async fn encode_payload_blocking(
    payload_mode: &str,
    path: String,
    warped_image: Option<std::sync::Arc<DynamicImage>>,
) -> Result<(Vec<u8>, String, String), String> {
    let payload_mode = payload_mode.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let (bytes, filename) = if payload_mode == "raw" {
            let raw = std::fs::read(&path).map_err(|e| e.to_string())?;
            let name = Path::new(&path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "source.raw".into());
            (raw, name)
        } else {
            let warped = warped_image.expect("warped image required for non-raw payload");
            let tiff = encode_linear_tiff(warped.as_ref())?;
            (tiff, "source.tiff".to_string())
        };
        let hash = blake3::hash(&bytes).to_hex().to_string();
        Ok::<_, String>((bytes, filename, hash))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Upload the current payload (or reuse a cached source via the blake3 memo)
/// and return the gateway `source_id`.
#[allow(clippy::too_many_arguments)]
async fn resolve_source_id(
    state: &tauri::State<'_, AppState>,
    client: &reqwest::Client,
    base: &str,
    token: Option<&str>,
    payload_mode: &str,
    path: &str,
    warped_image: Option<std::sync::Arc<DynamicImage>>,
    client_dims: Option<(u32, u32)>,
    exif_json: Option<String>,
    rrdata_json: Option<String>,
) -> Result<String, String> {
    let (bytes, filename, hash) =
        encode_payload_blocking(payload_mode, path.to_string(), warped_image).await?;

    let memo_hit = {
        let memo = state.remote_mask_source_memo.lock().unwrap();
        memo.as_ref()
            .filter(|(h, _)| *h == hash)
            .map(|(_, sid)| sid.clone())
    };
    if let Some(sid) = memo_hit {
        return Ok(sid);
    }

    let sid = upload_source(
        client,
        base,
        token,
        bytes,
        &filename,
        exif_json,
        rrdata_json,
        client_dims,
    )
    .await?;
    *state.remote_mask_source_memo.lock().unwrap() = Some((hash, sid.clone()));
    Ok(sid)
}

#[tauri::command]
pub async fn generate_remote_ai_mask(
    request: RemoteMaskRequest,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Value, String> {
    let settings = load_settings(app_handle.clone())?;
    let (base, token) = gateway_base(&app_handle)?;
    let client = reqwest::Client::new();

    emit_status(
        &app_handle,
        MaskStatusEvent {
            sub_mask_id: request.sub_mask_id.clone(),
            stage: "uploading".into(),
            queue_position: None,
            progress: None,
            detail: None,
        },
    );

    // ---- payload ----
    let payload_mode = settings.remote_mask_payload.as_deref().unwrap_or("tiff");
    let exif_json = gather_exif(&request.path);
    let rrdata_json = if settings.remote_mask_include_rrdata.unwrap_or(true) {
        gather_rrdata(&request.path)
    } else {
        None
    };

    // The warped image comes from AppState's cache (same image local AI
    // masks use). For "raw" payloads we still fetch it, purely to report
    // client dims so the gateway can reconcile against the original RAW.
    let warped_image = get_cached_full_warped_image(&state, &request.js_adjustments)?;
    let client_dims = if payload_mode == "raw" {
        Some((warped_image.width(), warped_image.height()))
    } else {
        None
    };
    let warped_for_encode = if payload_mode == "raw" {
        None
    } else {
        Some(warped_image)
    };

    let source_id = resolve_source_id(
        &state,
        &client,
        &base,
        token.as_deref(),
        payload_mode,
        &request.path,
        warped_for_encode.clone(),
        client_dims,
        exif_json.clone(),
        rrdata_json.clone(),
    )
    .await?;

    // ---- enqueue (with single transparent retry on 410 Gone) ----
    let params = MaskJobParams {
        mode: request.mode.clone(),
        query: request.query.clone(),
        points: request.points.clone(),
        roi_mask_b64: request.roi_mask_b64.clone(),
        preset: request.preset.clone(),
        agentic: Some(
            request
                .agentic
                .unwrap_or(settings.remote_mask_agentic_default.unwrap_or(false)),
        ),
        backend: settings.remote_mask_backend.clone(),
        sam3_multirep: request.sam3_multirep,
    };

    async fn submit_job(
        client: &reqwest::Client,
        base: &str,
        token: Option<&str>,
        source_id: &str,
        params: &MaskJobParams,
    ) -> Result<reqwest::Response, String> {
        let mut req = client
            .post(format!("{}/jobs/mask", base))
            .json(&serde_json::json!({"source_id": source_id, "params": params}));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        req.send().await.map_err(|e| e.to_string())
    }

    let mut resp = submit_job(&client, &base, token.as_deref(), &source_id, &params).await?;

    if resp.status().as_u16() == 410 {
        // Evicted server-side: the source cache makes a re-upload cheap, so
        // clear the memo and retry ONCE transparently rather than surfacing
        // an error to the user.
        *state.remote_mask_source_memo.lock().unwrap() = None;
        let new_source_id = resolve_source_id(
            &state,
            &client,
            &base,
            token.as_deref(),
            payload_mode,
            &request.path,
            warped_for_encode,
            client_dims,
            exif_json,
            rrdata_json,
        )
        .await?;
        resp = submit_job(&client, &base, token.as_deref(), &new_source_id, &params).await?;
    }

    if !resp.status().is_success() {
        return Err(format!("job submit failed: HTTP {}", resp.status()));
    }
    let job: JobSubmitted = resp.json().await.map_err(|e| e.to_string())?;
    {
        let mut cur = state.remote_mask_current_job.lock().unwrap();
        *cur = Some(job.job_id.clone());
    }

    // ---- poll ----
    let mut delay = std::time::Duration::from_millis(500);
    loop {
        tokio::time::sleep(delay).await;
        delay = std::cmp::min(delay * 2, std::time::Duration::from_secs(2));
        let mut req = client.get(format!("{}/jobs/{}", base, job.job_id));
        if let Some(t) = token.as_deref() {
            req = req.bearer_auth(t);
        }
        let st: JobStatus = req
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        match st.status.as_str() {
            "queued" | "running" => {
                emit_status(
                    &app_handle,
                    MaskStatusEvent {
                        sub_mask_id: request.sub_mask_id.clone(),
                        stage: st.status.clone(),
                        queue_position: st.queue_position,
                        progress: st.progress,
                        detail: None,
                    },
                );
            }
            "done" => {
                let r = st.result.ok_or("done without result")?;
                emit_status(
                    &app_handle,
                    MaskStatusEvent {
                        sub_mask_id: request.sub_mask_id.clone(),
                        stage: "done".into(),
                        queue_position: None,
                        progress: Some(1.0),
                        detail: None,
                    },
                );
                {
                    let mut cur = state.remote_mask_current_job.lock().unwrap();
                    *cur = None;
                }
                return Ok(serde_json::json!({
                    "maskDataBase64": format!("data:image/png;base64,{}", r.mask_png_b64),
                    "rotation": request.rotation,
                    "flipHorizontal": request.flip_horizontal,
                    "flipVertical": request.flip_vertical,
                    "orientationSteps": request.orientation_steps,
                    "mode": request.mode,
                    "query": request.query,
                    "preset": request.preset,
                    "agentic": params.agentic,
                    "backend": params.backend,
                    "alignment": r.alignment,
                    "labels": r.labels,
                }));
            }
            "cancelled" => {
                let mut cur = state.remote_mask_current_job.lock().unwrap();
                *cur = None;
                return Err("cancelled".into());
            }
            _ => {
                let detail = st
                    .error
                    .as_ref()
                    .and_then(|e| e.get("kind").and_then(|k| k.as_str()))
                    .unwrap_or("error")
                    .to_string();
                emit_status(
                    &app_handle,
                    MaskStatusEvent {
                        sub_mask_id: request.sub_mask_id.clone(),
                        stage: "error".into(),
                        queue_position: None,
                        progress: None,
                        detail: Some(detail.clone()),
                    },
                );
                let mut cur = state.remote_mask_current_job.lock().unwrap();
                *cur = None;
                return Err(format!("mask job failed: {} — {:?}", detail, st.error));
            }
        }
    }
}

#[tauri::command]
pub async fn cancel_remote_ai_mask(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let job_id = { state.remote_mask_current_job.lock().unwrap().clone() };
    let Some(job_id) = job_id else {
        return Ok(());
    };
    let (base, token) = gateway_base(&app_handle)?;
    let client = reqwest::Client::new();
    let mut req = client.delete(format!("{}/jobs/{}", base, job_id));
    if let Some(t) = token.as_deref() {
        req = req.bearer_auth(t);
    }
    req.send().await.map_err(|e| e.to_string())?;
    *state.remote_mask_current_job.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
pub async fn check_remote_mask_backend(app_handle: tauri::AppHandle) -> Result<Value, String> {
    let (base, token) = gateway_base(&app_handle)?;
    let client = reqwest::Client::new();
    let mut req = client
        .get(format!("{}/health", base))
        .timeout(std::time::Duration::from_secs(3));
    if let Some(t) = token.as_deref() {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let health: Value = resp.json().await.map_err(|e| e.to_string())?;
    let has_mask = health
        .get("capabilities")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().any(|v| v == "mask"))
        .unwrap_or(false);
    Ok(serde_json::json!({"available": has_mask, "health": health}))
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
            sam3_multirep: None,
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

    #[test]
    fn mask_request_omits_sam3_multirep_when_none() {
        let req = MaskJobParams {
            mode: "preset".into(),
            query: None,
            points: None,
            roi_mask_b64: None,
            preset: Some("subject".into()),
            agentic: None,
            backend: None,
            sam3_multirep: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("sam3_multirep").is_none());
        assert!(v.get("agentic").is_none());
        assert!(v.get("backend").is_none());
    }

    #[test]
    fn mask_request_includes_sam3_multirep_when_set() {
        let req = MaskJobParams {
            mode: "prompt".into(),
            query: Some("subject".into()),
            points: None,
            roi_mask_b64: None,
            preset: None,
            agentic: Some(true),
            backend: Some("sam3".into()),
            sam3_multirep: Some(true),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["sam3_multirep"], true);
        assert_eq!(v["backend"], "sam3");
    }

    #[test]
    fn normalize_base_address_adds_http_to_bare_host() {
        assert_eq!(
            normalize_base_address("inferno:5000"),
            "http://inferno:5000"
        );
        assert_eq!(normalize_base_address("localhost"), "http://localhost");
    }

    #[test]
    fn normalize_base_address_preserves_existing_scheme() {
        assert_eq!(
            normalize_base_address("http://inferno:5000"),
            "http://inferno:5000"
        );
        assert_eq!(
            normalize_base_address("https://gateway.example.com"),
            "https://gateway.example.com"
        );
    }

    #[test]
    fn remote_mask_request_deserializes_camel_case_with_none_omitted() {
        let json = serde_json::json!({
            "subMaskId": "sm-1",
            "path": "/tmp/photo.arw",
            "mode": "prompt",
            "query": "the dog.",
            "points": null,
            "roiMaskB64": null,
            "preset": null,
            "agentic": null,
            "sam3Multirep": null,
            "rotation": 0.0,
            "flipHorizontal": false,
            "flipVertical": false,
            "orientationSteps": 0,
            "jsAdjustments": {}
        });
        let req: RemoteMaskRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.sub_mask_id, "sm-1");
        assert_eq!(req.mode, "prompt");
        assert_eq!(req.query.as_deref(), Some("the dog."));
        assert!(req.points.is_none());
        assert!(req.roi_mask_b64.is_none());
        assert!(req.sam3_multirep.is_none());
    }
}
