//! Remote AI masking via the rr-ai-gateway (see docs/superpowers/specs/
//! 2026-07-07-remote-ai-masking-design.md). Upload once (content-addressed),
//! enqueue a mask job, poll, return parameters for a `remote-ai` sub-mask.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::time::Duration;

use image::codecs::tiff::TiffEncoder;
use image::{DynamicImage, ExtendedColorType, ImageEncoder, ImageFormat};
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::Emitter;

use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::get_cached_full_warped_image;

/// Max entries kept in the source memo (blake3 hex -> gateway source_id).
/// A real LRU is overkill here: eviction just means the next upload for that
/// content hash pays for a redundant re-upload (or a 410-retry), never a
/// correctness problem.
const REMOTE_MASK_MEMO_CAP: usize = 8;

/// Insert `(hash, source_id)` into the memo map, evicting an arbitrary entry
/// first if the map is already at capacity. Factored out as a pure function
/// so the capping behavior is unit-testable without touching AppState/mutexes.
fn memo_insert_capped(map: &mut HashMap<String, String>, hash: String, source_id: String) {
    if map.len() >= REMOTE_MASK_MEMO_CAP && !map.contains_key(&hash) {
        if let Some(evict_key) = map.keys().next().cloned() {
            map.remove(&evict_key);
        }
    }
    map.insert(hash, source_id);
}

/// Wall-clock ceiling for the poll loop. The gateway's own job timeout is
/// 600s; this is generous enough to also cover queue wait ahead of that.
const POLL_DEADLINE: Duration = Duration::from_secs(900);

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

/// Un-orient a point from ORIENTED display space (where the frontend captures
/// clicks/paint, with width/height swapped for `steps` 1/3) into the coarse-
/// UNROTATED payload space that the uploaded TIFF actually lives in.
///
/// This is exactly the inverse of the RETURNED-mask transform in
/// `mask_generation::generate_ai_bitmap_from_full_mask`: that path samples the
/// received (unoriented) mask for each oriented output pixel via a flip-then-
/// orientation-match; here we apply that identical oriented -> unoriented map
/// so the OUTBOUND spatial inputs (points, ROI) match the payload the gateway
/// segments. `oriented_w`/`oriented_h` are the oriented display dims
/// (== `coarse_rotated_w`/`coarse_rotated_h` in the inbound code).
pub fn unorient_point(
    x: f64,
    y: f64,
    oriented_w: f64,
    oriented_h: f64,
    steps: u8,
    flip_h: bool,
    flip_v: bool,
) -> (f64, f64) {
    // Flip is undone first, in oriented (coarse-rotated) space, matching the
    // inbound order (flip on x_unrotated -> x_unflipped, then the match).
    let x_unflipped = if flip_h { oriented_w - x } else { x };
    let y_unflipped = if flip_v { oriented_h - y } else { y };
    match steps % 4 {
        0 => (x_unflipped, y_unflipped),
        1 => (y_unflipped, oriented_w - x_unflipped),
        2 => (oriented_w - x_unflipped, oriented_h - y_unflipped),
        3 => (oriented_h - y_unflipped, x_unflipped),
        _ => unreachable!(),
    }
}

/// Un-orient an ROI mask PNG from ORIENTED display space into coarse-UNROTATED
/// payload space, the raster analogue of [`unorient_point`]: decode, apply the
/// inverse orientation (flip first, then rotation) via `image::imageops`,
/// re-encode PNG. No-op fast path for `steps == 0 && !flip_h && !flip_v`.
pub fn unorient_roi_png(
    png_bytes: &[u8],
    steps: u8,
    flip_h: bool,
    flip_v: bool,
) -> Result<Vec<u8>, String> {
    use image::imageops;
    let img = image::load_from_memory(png_bytes).map_err(|e| e.to_string())?;
    let mut img = img;
    // Flip first (oriented space), mirroring unorient_point's order.
    if flip_h {
        img = DynamicImage::ImageRgba8(imageops::flip_horizontal(&img));
    }
    if flip_v {
        img = DynamicImage::ImageRgba8(imageops::flip_vertical(&img));
    }
    // Then the inverse orientation rotation. steps is the number of CW quarter
    // turns applied to the UNORIENTED image to reach the oriented display; the
    // inverse (oriented -> unoriented) rotates the opposite way.
    let out = match steps % 4 {
        0 => img,
        1 => DynamicImage::ImageRgba8(imageops::rotate270(&img)),
        2 => DynamicImage::ImageRgba8(imageops::rotate180(&img)),
        3 => DynamicImage::ImageRgba8(imageops::rotate90(&img)),
        _ => unreachable!(),
    };
    let mut buf = Cursor::new(Vec::new());
    out.write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

/// Un-orient an ROI mask supplied as base64 (optionally a `data:image/png;
/// base64,...` data URL): strip any prefix, decode, un-orient via
/// [`unorient_roi_png`], re-encode, and re-wrap with the same prefix style the
/// caller used. Returns bare/prefixed base64 matching the input.
pub fn unorient_roi_b64(
    roi: &str,
    steps: u8,
    flip_h: bool,
    flip_v: bool,
) -> Result<String, String> {
    use base64::{engine::general_purpose, Engine as _};
    let (prefix, b64) = match roi.find(',') {
        Some(idx) if roi.starts_with("data:") => (Some(&roi[..=idx]), &roi[idx + 1..]),
        _ => (None, roi),
    };
    let bytes = general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| e.to_string())?;
    let out = unorient_roi_png(&bytes, steps, flip_h, flip_v)?;
    let encoded = general_purpose::STANDARD.encode(out);
    Ok(match prefix {
        Some(p) => format!("{}{}", p, encoded),
        None => encoded,
    })
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

/// Everything gathered by `encode_payload_blocking`: the upload bytes/filename,
/// the content hash used for the source memo, and the sidecar EXIF/rrdata JSON
/// (also read from disk, so they ride along in the same blocking closure).
struct EncodedPayload {
    bytes: Vec<u8>,
    filename: String,
    hash: String,
    exif_json: Option<String>,
    rrdata_json: Option<String>,
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

/// Encode the upload payload (TIFF-encode + hash) and gather sidecar
/// EXIF/rrdata off the async executor: with a 150-300MB warped-image buffer
/// plus potentially tens-of-MB `std::fs::read`s for EXIF/rrdata sidecars,
/// doing any of this inline would stall every other task on the Tokio
/// runtime.
async fn encode_payload_blocking(
    payload_mode: &str,
    path: String,
    warped_image: Option<std::sync::Arc<DynamicImage>>,
    include_rrdata: bool,
) -> Result<EncodedPayload, String> {
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
        let exif_json = gather_exif(&path);
        let rrdata_json = if include_rrdata { gather_rrdata(&path) } else { None };
        Ok::<_, String>(EncodedPayload {
            bytes,
            filename,
            hash,
            exif_json,
            rrdata_json,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Upload the current payload (or reuse a cached source via the blake3 memo)
/// and return the gateway `source_id` along with the content hash used to
/// look it up, so callers can evict precisely on a stale-source 410.
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
    include_rrdata: bool,
) -> Result<(String, String), String> {
    let encoded = encode_payload_blocking(
        payload_mode,
        path.to_string(),
        warped_image,
        include_rrdata,
    )
    .await?;

    let memo_hit = {
        let memo = state.remote_mask_source_memo.lock().unwrap();
        memo.get(&encoded.hash).cloned()
    };
    if let Some(sid) = memo_hit {
        return Ok((encoded.hash, sid));
    }

    let sid = upload_source(
        client,
        base,
        token,
        encoded.bytes,
        &encoded.filename,
        encoded.exif_json,
        encoded.rrdata_json,
        client_dims,
    )
    .await?;
    {
        let mut memo = state.remote_mask_source_memo.lock().unwrap();
        memo_insert_capped(&mut memo, encoded.hash.clone(), sid.clone());
    }
    Ok((encoded.hash, sid))
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
    let include_rrdata = settings.remote_mask_include_rrdata.unwrap_or(true);

    // The warped image comes from AppState's cache (same image local AI
    // masks use). For "raw" payloads we still fetch it, purely to report
    // client dims so the gateway can reconcile against the original RAW.
    let warped_image = get_cached_full_warped_image(&state, &request.js_adjustments)?;
    // The warped image is the coarse-UNROTATED payload space. The frontend
    // captured points/ROI in ORIENTED display space, whose dims are swapped for
    // odd orientation_steps. Derive the oriented dims so we can un-orient the
    // outbound spatial inputs back into payload space (see unorient_point).
    let (unoriented_w, unoriented_h) = (warped_image.width(), warped_image.height());
    let (oriented_w, oriented_h) = if request.orientation_steps % 2 == 1 {
        (unoriented_h as f64, unoriented_w as f64)
    } else {
        (unoriented_w as f64, unoriented_h as f64)
    };
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

    let (source_hash, source_id) = resolve_source_id(
        &state,
        &client,
        &base,
        token.as_deref(),
        payload_mode,
        &request.path,
        warped_for_encode.clone(),
        client_dims,
        include_rrdata,
    )
    .await?;

    // ---- un-orient outbound spatial inputs to payload space ----
    // The uploaded payload lives in coarse-UNROTATED space; the frontend's
    // points/ROI are in ORIENTED display space. Transform both to match (no-op
    // when steps==0 && no flips). The RETURNED mask is un-oriented downstream
    // by generate_ai_bitmap_from_full_mask, so only the outbound side needs it.
    let needs_unorient = request.orientation_steps % 4 != 0
        || request.flip_horizontal
        || request.flip_vertical;

    let unoriented_points = request.points.as_ref().map(|pts| {
        pts.iter()
            .map(|p| {
                let (ux, uy) = unorient_point(
                    p[0],
                    p[1],
                    oriented_w,
                    oriented_h,
                    request.orientation_steps,
                    request.flip_horizontal,
                    request.flip_vertical,
                );
                [ux, uy, p[2]]
            })
            .collect::<Vec<[f64; 3]>>()
    });

    let unoriented_roi = if needs_unorient {
        match request.roi_mask_b64.as_ref() {
            Some(roi) => Some(unorient_roi_b64(
                roi,
                request.orientation_steps,
                request.flip_horizontal,
                request.flip_vertical,
            )?),
            None => None,
        }
    } else {
        request.roi_mask_b64.clone()
    };

    // ---- enqueue (with single transparent retry on 410 Gone) ----
    let params = MaskJobParams {
        mode: request.mode.clone(),
        query: request.query.clone(),
        points: unoriented_points,
        roi_mask_b64: unoriented_roi,
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
        // drop only the offending hash's memo entry and retry ONCE
        // transparently rather than surfacing an error to the user (other
        // memoized sources are still valid, no need to clear the whole map).
        state
            .remote_mask_source_memo
            .lock()
            .unwrap()
            .remove(&source_hash);
        let (_, new_source_id) = resolve_source_id(
            &state,
            &client,
            &base,
            token.as_deref(),
            payload_mode,
            &request.path,
            warped_for_encode,
            client_dims,
            include_rrdata,
        )
        .await?;
        resp = submit_job(&client, &base, token.as_deref(), &new_source_id, &params).await?;
    }

    if !resp.status().is_success() {
        return Err(format!("job submit failed: HTTP {}", resp.status()));
    }
    let job: JobSubmitted = resp.json().await.map_err(|e| e.to_string())?;
    {
        let mut jobs = state.remote_mask_jobs.lock().unwrap();
        jobs.insert(request.sub_mask_id.clone(), job.job_id.clone());
    }

    // ---- poll ----
    // The whole polling section is wrapped in an inner async block so that,
    // regardless of which path it exits through (success, cancellation,
    // terminal error, or a transient network/deserialize error via `?`),
    // this sub-mask's entry in `remote_mask_jobs` is removed exactly once,
    // unconditionally, right after the block finishes. This avoids leaving a
    // stale job id in AppState if a poll request itself fails, while leaving
    // other sub-masks' concurrent jobs untouched.
    let poll_start = std::time::Instant::now();
    let outcome: Result<Value, String> = async {
        let mut delay = std::time::Duration::from_millis(500);
        loop {
            if poll_start.elapsed() >= POLL_DEADLINE {
                return Err("mask job timed out waiting for the gateway".into());
            }
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
                        "width": r.width,
                        "height": r.height,
                        "timings": r.timings,
                    }));
                }
                "cancelled" => {
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
                    return Err(format!("mask job failed: {} — {:?}", detail, st.error));
                }
            }
        }
    }
    .await;

    state
        .remote_mask_jobs
        .lock()
        .unwrap()
        .remove(&request.sub_mask_id);
    outcome
}

#[tauri::command]
pub async fn cancel_remote_ai_mask(
    sub_mask_id: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let job_id = {
        state
            .remote_mask_jobs
            .lock()
            .unwrap()
            .remove(&sub_mask_id)
    };
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

    #[test]
    fn memo_insert_capped_stays_at_cap_and_evicts_something() {
        let mut map: HashMap<String, String> = HashMap::new();
        for i in 0..REMOTE_MASK_MEMO_CAP {
            memo_insert_capped(&mut map, format!("hash{i}"), format!("sid{i}"));
        }
        assert_eq!(map.len(), REMOTE_MASK_MEMO_CAP);

        // One more insert beyond the cap must evict something rather than
        // growing the map unbounded.
        memo_insert_capped(&mut map, "hash-new".into(), "sid-new".into());
        assert_eq!(map.len(), REMOTE_MASK_MEMO_CAP);
        assert_eq!(map.get("hash-new").map(String::as_str), Some("sid-new"));
    }

    #[test]
    fn memo_insert_capped_updates_existing_key_without_evicting() {
        let mut map: HashMap<String, String> = HashMap::new();
        for i in 0..REMOTE_MASK_MEMO_CAP {
            memo_insert_capped(&mut map, format!("hash{i}"), format!("sid{i}"));
        }
        // Re-inserting an existing hash with a new source_id must not evict
        // any other entry, since the map isn't actually growing.
        memo_insert_capped(&mut map, "hash0".into(), "sid0-updated".into());
        assert_eq!(map.len(), REMOTE_MASK_MEMO_CAP);
        assert_eq!(
            map.get("hash0").map(String::as_str),
            Some("sid0-updated")
        );
    }

    // --- geometry: un-orient outbound points/ROI to payload (unoriented) space ---

    /// Mirror of the INBOUND orientation math in
    /// `mask_generation::generate_ai_bitmap_from_full_mask` (scale=1, no crop,
    /// no fine rotation): given an ORIENTED display point and the oriented
    /// dims, produce the UNORIENTED (payload) point. `unorient_point` must be
    /// exactly this function; the round-trip below composes the two and must
    /// recover the identity.
    fn inbound_oriented_to_unoriented(
        xo: f64,
        yo: f64,
        oriented_w: f64,
        oriented_h: f64,
        steps: u8,
        flip_h: bool,
        flip_v: bool,
    ) -> (f64, f64) {
        // Flip is applied first, in oriented (coarse-rotated) space.
        let x_unflipped = if flip_h { oriented_w - xo } else { xo };
        let y_unflipped = if flip_v { oriented_h - yo } else { yo };
        // Then the orientation match maps oriented -> unoriented coarse.
        // Note: oriented_w == coarse_rotated_w, oriented_h == coarse_rotated_h.
        match steps {
            0 => (x_unflipped, y_unflipped),
            1 => (y_unflipped, oriented_w - x_unflipped),
            2 => (oriented_w - x_unflipped, oriented_h - y_unflipped),
            3 => (oriented_h - y_unflipped, x_unflipped),
            _ => (x_unflipped, y_unflipped),
        }
    }

    #[test]
    fn unorient_point_is_noop_for_steps0_no_flip() {
        for &(x, y) in &[(0.0, 0.0), (10.0, 20.0), (5999.0, 3999.0)] {
            let (ux, uy) = unorient_point(x, y, 6000.0, 4000.0, 0, false, false);
            assert!((ux - x).abs() < 1e-9 && (uy - y).abs() < 1e-9);
        }
    }

    #[test]
    fn unorient_point_maps_oriented_corners_to_payload_corners() {
        // Unoriented payload is 4000x6000 (portrait). For steps=1 and steps=3
        // the oriented display is 6000x4000 (landscape); for steps=0/2 it stays
        // 4000x6000. Corner-to-corner checks pin the exact rotation direction.
        // steps=1: oriented 6000x4000 -> unoriented 4000x6000
        let (ux, uy) = unorient_point(0.0, 0.0, 6000.0, 4000.0, 1, false, false);
        assert!((ux - 0.0).abs() < 0.5 && (uy - 6000.0).abs() < 0.5);
        let (ux, uy) = unorient_point(6000.0, 0.0, 6000.0, 4000.0, 1, false, false);
        assert!((ux - 0.0).abs() < 0.5 && (uy - 0.0).abs() < 0.5);
        // steps=2: 180, dims unchanged (say 4000x6000 oriented)
        let (ux, uy) = unorient_point(0.0, 0.0, 4000.0, 6000.0, 2, false, false);
        assert!((ux - 4000.0).abs() < 0.5 && (uy - 6000.0).abs() < 0.5);
        // steps=3: oriented 6000x4000 -> unoriented 4000x6000
        let (ux, uy) = unorient_point(0.0, 0.0, 6000.0, 4000.0, 3, false, false);
        assert!((ux - 4000.0).abs() < 0.5 && (uy - 0.0).abs() < 0.5);
    }

    #[test]
    fn unorient_point_roundtrips_with_inbound_for_all_steps_and_flips() {
        // For each steps/flip combo: un-orient an oriented point to payload
        // space, then feed it back through the inbound math and require the
        // original oriented point (within 0.5px).
        let cases: &[(f64, f64, f64, f64, u8)] = &[
            (6000.0, 4000.0, 6000.0, 4000.0, 0), // oriented == unoriented dims
            (6000.0, 4000.0, 4000.0, 6000.0, 1), // 90: swapped
            (4000.0, 6000.0, 4000.0, 6000.0, 2), // 180: unchanged
            (6000.0, 4000.0, 4000.0, 6000.0, 3), // 270: swapped
        ];
        for &(ow, oh, _uw, _uh, steps) in cases {
            for &(flip_h, flip_v) in &[(false, false), (true, false), (false, true), (true, true)] {
                for &(xo, yo) in &[(0.0, 0.0), (123.5, 987.25), (ow - 1.0, oh - 1.0), (ow / 2.0, oh / 3.0)] {
                    let (ux, uy) = unorient_point(xo, yo, ow, oh, steps, flip_h, flip_v);
                    let (rx, ry) =
                        inbound_oriented_to_unoriented(xo, yo, ow, oh, steps, flip_h, flip_v);
                    // unorient_point IS the inbound oriented->unoriented map.
                    assert!(
                        (ux - rx).abs() < 0.5 && (uy - ry).abs() < 0.5,
                        "steps={steps} flip=({flip_h},{flip_v}) pt=({xo},{yo}): got ({ux},{uy}) want ({rx},{ry})"
                    );
                }
            }
        }
    }

    /// Build a tiny grayscale PNG with a single white pixel at (px, py).
    fn png_with_white_pixel(w: u32, h: u32, px: u32, py: u32) -> Vec<u8> {
        let mut img = image::GrayImage::new(w, h);
        img.put_pixel(px, py, image::Luma([255]));
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    /// Find the (x, y) of the (first) white pixel in a decoded grayscale PNG.
    fn white_pixel_of(png: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(png).unwrap().to_luma8();
        for y in 0..img.height() {
            for x in 0..img.width() {
                if img.get_pixel(x, y).0[0] > 127 {
                    return (x, y);
                }
            }
        }
        panic!("no white pixel found");
    }

    #[test]
    fn unorient_roi_png_moves_white_pixel_to_payload_position() {
        // Oriented ROI is 3x2 (steps 0/2) or 3x2 -> 2x3 (steps 1/3).
        // The white pixel must land where unorient_point predicts.
        let (ow, oh) = (3u32, 2u32);
        for steps in 0u8..4 {
            for &(flip_h, flip_v) in &[(false, false), (true, false), (false, true)] {
                let (px, py) = (2u32, 0u32); // oriented top-right pixel
                let png = png_with_white_pixel(ow, oh, px, py);
                let out = unorient_roi_png(&png, steps, flip_h, flip_v).unwrap();

                // Predict via the pixel-centered point transform.
                let (fx, fy) = unorient_point(
                    px as f64 + 0.5,
                    py as f64 + 0.5,
                    ow as f64,
                    oh as f64,
                    steps,
                    flip_h,
                    flip_v,
                );
                let (ux, uy) = white_pixel_of(&out);
                let ex = fx.floor().max(0.0) as u32;
                let ey = fy.floor().max(0.0) as u32;
                assert_eq!(
                    (ux, uy),
                    (ex, ey),
                    "steps={steps} flip=({flip_h},{flip_v}): white at ({ux},{uy}) want ({ex},{ey})"
                );
            }
        }
    }

    #[test]
    fn unorient_roi_png_is_noop_for_steps0_no_flip() {
        let png = png_with_white_pixel(3, 2, 2, 0);
        let out = unorient_roi_png(&png, 0, false, false).unwrap();
        assert_eq!(white_pixel_of(&out), (2, 0));
        let dims = image::load_from_memory(&out).unwrap();
        assert_eq!((dims.width(), dims.height()), (3, 2));
    }

    #[test]
    fn unorient_roi_b64_preserves_data_url_prefix_and_transforms() {
        use base64::{engine::general_purpose, Engine as _};
        let png = png_with_white_pixel(3, 2, 2, 0);
        let data_url = format!(
            "data:image/png;base64,{}",
            general_purpose::STANDARD.encode(&png)
        );
        // steps=1 must actually move the pixel and keep the data-URL prefix.
        let out = unorient_roi_b64(&data_url, 1, false, false).unwrap();
        assert!(out.starts_with("data:image/png;base64,"));
        let decoded = general_purpose::STANDARD
            .decode(out.split(',').nth(1).unwrap())
            .unwrap();
        let (ux, uy) = white_pixel_of(&decoded);
        let (fx, fy) = unorient_point(2.5, 0.5, 3.0, 2.0, 1, false, false);
        assert_eq!((ux, uy), (fx.floor() as u32, fy.floor() as u32));
    }

    #[test]
    fn unorient_roi_b64_handles_bare_base64() {
        use base64::{engine::general_purpose, Engine as _};
        let png = png_with_white_pixel(3, 2, 2, 0);
        let bare = general_purpose::STANDARD.encode(&png);
        let out = unorient_roi_b64(&bare, 0, false, false).unwrap();
        assert!(!out.starts_with("data:"));
        let decoded = general_purpose::STANDARD.decode(&out).unwrap();
        assert_eq!(white_pixel_of(&decoded), (2, 0));
    }
}
