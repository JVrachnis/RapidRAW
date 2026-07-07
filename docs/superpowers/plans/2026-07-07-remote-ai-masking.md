# RapidRAW Remote AI Masking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `remote-ai` mask type to RapidRAW that ships the editor's image (16-bit linear TIFF, or the original RAW) plus EXIF and optional `.rrdata` to the rr-ai-gateway on inferno and stores the returned alpha as a normal sub-mask bitmap.

**Architecture:** One new Rust module `remote_masking.rs` (payload assembly, gateway HTTP client, upload→enqueue→poll with Tauri progress events); one new dispatch arm in `mask_generation.rs` reusing the existing AI-bitmap rasterization; settings fields beside the existing connector address; frontend gets a `RemoteAi` mask type whose panel offers prompt / points / paint / preset modes.

**Tech Stack:** Rust (tauri, reqwest multipart, image/tiff, blake3, serde), React + TypeScript frontend.

**Spec:** `docs/superpowers/specs/2026-07-07-remote-ai-masking-design.md`
**Repo/branch:** `~/Apps/RapidRawFork/RapidRAW`, branch `feat/remote-ai-masking`
**Depends on:** rr-ai-gateway plan (API must exist; for E2E, gateway deployed on inferno)

## File Structure

```
src-tauri/src/remote_masking.rs        NEW: types, TIFF export, gateway client, tauri commands
src-tauri/src/app_settings.rs          MODIFY: 5 new optional settings fields
src-tauri/src/mask_generation.rs       MODIFY: "remote-ai" dispatch arm (~line 1305)
src-tauri/src/lib.rs                   MODIFY: mod + 3 commands in invoke_handler (~line 2248)
src/components/panel/right/Masks.tsx   MODIFY: enum + name + icon
src/utils/maskUtils.ts                 MODIFY: createSubMask case
src/components/ui/AppProperties.tsx    MODIFY: Invokes enum entries
src/hooks/useRemoteAiMasking.ts        NEW: frontend job flow hook
src/components/panel/right/RemoteMaskControls.tsx  NEW: mode UI panel
src/components/panel/right/MasksPanel.tsx MODIFY: render RemoteMaskControls for remote-ai
src/components/panel/SettingsPanel.tsx MODIFY: settings inputs (~line 522)
src/i18n/locales/en.json               MODIFY: strings
docs/superpowers/testing/remote-mask-e2e.md  NEW: manual E2E checklist
```

Build/test commands: `cd src-tauri && cargo test remote_masking` (Rust), `npm run build` (frontend type-check + bundle). Run app: `npm run tauri dev`.

---

### Task 1: Settings fields

**Files:**
- Modify: `src-tauri/src/app_settings.rs` (struct `AppSettings` around line 354, defaults around line 477)

- [ ] **Step 1: Add fields**

In the `AppSettings` struct, directly after `pub ai_connector_address: Option<String>,` (line ~355) add:
```rust
    #[serde(default)]
    pub remote_mask_address: Option<String>,
    #[serde(default)]
    pub remote_mask_payload: Option<String>, // "tiff" (default) | "raw"
    #[serde(default)]
    pub remote_mask_include_rrdata: Option<bool>,
    #[serde(default)]
    pub remote_mask_backend: Option<String>, // "sam2" (default) | "sam3"
    #[serde(default)]
    pub remote_mask_agentic_default: Option<bool>,
```

In the `Default for AppSettings` impl (where `processing_backend: Some("auto".to_string())` lives, line ~477) add:
```rust
            remote_mask_address: None,
            remote_mask_payload: Some("tiff".to_string()),
            remote_mask_include_rrdata: Some(true),
            remote_mask_backend: Some("sam2".to_string()),
            remote_mask_agentic_default: Some(false),
```

Serde renames to camelCase follow the struct's existing container attribute — verify the struct has `#[serde(rename_all = "camelCase")]`; if fields are snake_case-serialized instead, match whatever the sibling fields do (the frontend reads `appSettings?.aiConnectorAddress`, so camelCase is expected).

- [ ] **Step 2: Compile**

Run: `cd src-tauri && cargo check` — Expected: clean (warnings ok)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/app_settings.rs
git commit -m "feat: settings for remote AI masking gateway"
```

---

### Task 2: remote_masking.rs — types + payload helpers (TDD)

**Files:**
- Create: `src-tauri/src/remote_masking.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod remote_masking;` next to `mod ai_connector;`)

- [ ] **Step 1: Write the failing tests**

Bottom of the new `src-tauri/src/remote_masking.rs` (create the file with ONLY imports + tests first):
```rust
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
        // 16-bit output
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
```

Add `mod remote_masking;` in `src-tauri/src/lib.rs` next to the other `mod` declarations (top of file, e.g. after `mod ai_connector;` — check line ~10-40 for the mod block).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test remote_masking` — Expected: compile FAIL (types missing)

- [ ] **Step 3: Implement types + helpers**

Top of `src-tauri/src/remote_masking.rs`:
```rust
//! Remote AI masking via the rr-ai-gateway (see docs/superpowers/specs/
//! 2026-07-07-remote-ai-masking-design.md). Upload once (content-addressed),
//! enqueue a mask job, poll, return parameters for a `remote-ai` sub-mask.

use std::io::Cursor;
use std::path::Path;

use base64::{Engine as _, engine::general_purpose};
use image::codecs::tiff::TiffEncoder;
use image::{DynamicImage, ExtendedColorType};
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

pub fn encode_linear_tiff(img: &DynamicImage) -> Result<Vec<u8>, String> {
    let rgb16 = img.to_rgb16();
    let (w, h) = (rgb16.width(), rgb16.height());
    let mut buf = Cursor::new(Vec::new());
    let encoder = TiffEncoder::new(&mut buf);
    // Rgb16 raw samples are u16; bytemuck them to &[u8] little-endian as the
    // encoder expects native order via write_image's u8 slice.
    let raw: &[u16] = rgb16.as_raw();
    let bytes: Vec<u8> = raw.iter().flat_map(|v| v.to_ne_bytes()).collect();
    encoder
        .write_image(&bytes, w, h, ExtendedColorType::Rgb16)
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}
```

If `image`'s TIFF encoder is not enabled in `src-tauri/Cargo.toml` (check `image = { ... features = [...] }` for `"tiff"`), add the feature. If `write_image`'s signature differs in the pinned image version, adapt (older: `encode(&bytes, w, h, ColorType::Rgb16)`); the test is the arbiter.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test remote_masking` — Expected: 3 PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/remote_masking.rs src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat: remote masking types + 16-bit linear TIFF export"
```

---

### Task 3: remote_masking.rs — gateway client + tauri commands

**Files:**
- Modify: `src-tauri/src/remote_masking.rs`

The HTTP flow is exercised end-to-end in the E2E checklist (Task 8); unit tests here cover the pure pieces (already done in Task 2). Keep functions small so review is easy.

- [ ] **Step 1: Implement the client + commands**

Append to `src-tauri/src/remote_masking.rs`:
```rust
use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::{get_cached_full_warped_image};
use reqwest::multipart;
use tauri::Emitter;

#[derive(Deserialize, Debug)]
struct SourceResponse {
    source_id: String,
}

#[derive(Deserialize, Debug)]
struct JobSubmitted {
    job_id: String,
    #[serde(default)]
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
    stage: String,             // "uploading" | "queued" | "running" | "done" | "error"
    queue_position: Option<u32>,
    progress: Option<f64>,
    detail: Option<String>,
}

fn emit_status(app: &tauri::AppHandle, ev: MaskStatusEvent) {
    let _ = app.emit("remote-mask-status", ev);
}

fn gateway_base(app_handle: &tauri::AppHandle) -> Result<(String, Option<String>), String> {
    let settings = load_settings(app_handle.clone()).map_err(|e| e.to_string())?;
    let addr = settings
        .remote_mask_address
        .filter(|s| !s.is_empty())
        .or(settings.ai_connector_address.clone())
        .ok_or("No AI backend address configured")?;
    let base = if addr.starts_with("http") { addr } else { format!("http://{}", addr) };
    // token handling mirrors ai_connector.rs — reuse its token source if one exists
    Ok((base, None))
}

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
    pub rotation: f32,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub orientation_steps: u8,
    pub js_adjustments: Value,
}

#[tauri::command]
pub async fn generate_remote_ai_mask(
    request: RemoteMaskRequest,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Value, String> {
    let settings = load_settings(app_handle.clone()).map_err(|e| e.to_string())?;
    let (base, token) = gateway_base(&app_handle)?;
    let client = reqwest::Client::new();

    emit_status(&app_handle, MaskStatusEvent {
        sub_mask_id: request.sub_mask_id.clone(), stage: "uploading".into(),
        queue_position: None, progress: None, detail: None });

    // ---- payload ----
    let payload_mode = settings.remote_mask_payload.as_deref().unwrap_or("tiff");
    let exif_json = gather_exif(&request.path);
    let rrdata_json = if settings.remote_mask_include_rrdata.unwrap_or(true) {
        gather_rrdata(&request.path)
    } else {
        None
    };

    let (bytes, filename, dims) = if payload_mode == "raw" {
        let raw = std::fs::read(&request.path).map_err(|e| e.to_string())?;
        let name = Path::new(&request.path)
            .file_name().map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "source.raw".into());
        // client dims = warped image dims so the gateway can reconcile
        let warped = get_cached_full_warped_image(&state, &request.js_adjustments)?;
        let d = (warped.width(), warped.height());
        (raw, name, Some(d))
    } else {
        let warped = get_cached_full_warped_image(&state, &request.js_adjustments)?;
        let tiff = encode_linear_tiff(warped.as_ref())?;
        (tiff, "source.tiff".to_string(), None)
    };

    // content-hash memo: skip re-upload when bytes unchanged
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let memo_hit = {
        let memo = state.remote_mask_source_memo.lock().unwrap();
        memo.as_ref().filter(|(h, _)| *h == hash).map(|(_, sid)| sid.clone())
    };
    let source_id = match memo_hit {
        Some(sid) => sid,
        None => {
            let sid = upload_source(&client, &base, token.as_deref(), bytes, &filename,
                                    exif_json, rrdata_json, dims).await?;
            *state.remote_mask_source_memo.lock().unwrap() = Some((hash, sid.clone()));
            sid
        }
    };

    // ---- enqueue ----
    let params = MaskJobParams {
        mode: request.mode.clone(),
        query: request.query.clone(),
        points: request.points.clone(),
        roi_mask_b64: request.roi_mask_b64.clone(),
        preset: request.preset.clone(),
        agentic: Some(request.agentic.unwrap_or(
            settings.remote_mask_agentic_default.unwrap_or(false))),
        backend: settings.remote_mask_backend.clone(),
    };
    let mut req = client
        .post(format!("{}/jobs/mask", base))
        .json(&serde_json::json!({"source_id": source_id, "params": params}));
    if let Some(t) = token.as_deref() {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if resp.status().as_u16() == 410 {
        // evicted server-side: drop memo; caller retries
        *state.remote_mask_source_memo.lock().unwrap() = None;
        return Err("source evicted on gateway; please retry".into());
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
        let st: JobStatus = req.send().await.map_err(|e| e.to_string())?
            .json().await.map_err(|e| e.to_string())?;
        match st.status.as_str() {
            "queued" | "running" => {
                emit_status(&app_handle, MaskStatusEvent {
                    sub_mask_id: request.sub_mask_id.clone(), stage: st.status.clone(),
                    queue_position: st.queue_position, progress: st.progress, detail: None });
            }
            "done" => {
                let r = st.result.ok_or("done without result")?;
                emit_status(&app_handle, MaskStatusEvent {
                    sub_mask_id: request.sub_mask_id.clone(), stage: "done".into(),
                    queue_position: None, progress: Some(1.0), detail: None });
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
            "cancelled" => return Err("cancelled".into()),
            _ => {
                let detail = st.error
                    .as_ref()
                    .and_then(|e| e.get("kind").and_then(|k| k.as_str()))
                    .unwrap_or("error").to_string();
                emit_status(&app_handle, MaskStatusEvent {
                    sub_mask_id: request.sub_mask_id.clone(), stage: "error".into(),
                    queue_position: None, progress: None, detail: Some(detail.clone()) });
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
    let job_id = state.remote_mask_current_job.lock().unwrap().clone();
    let Some(job_id) = job_id else { return Ok(()); };
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
    let mut req = client.get(format!("{}/health", base))
        .timeout(std::time::Duration::from_secs(3));
    if let Some(t) = token.as_deref() {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let health: Value = resp.json().await.map_err(|e| e.to_string())?;
    let has_mask = health.get("capabilities")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().any(|v| v == "mask"))
        .unwrap_or(false);
    Ok(serde_json::json!({"available": has_mask, "health": health}))
}
```

Two `AppState` additions (in `src-tauri/src/app_state.rs`, following the pattern of existing `Mutex` fields — find the struct and its `Default`/constructor):
```rust
    pub remote_mask_source_memo: std::sync::Mutex<Option<(String, String)>>, // (blake3, source_id)
    pub remote_mask_current_job: std::sync::Mutex<Option<String>>,
```
Initialize both with `Mutex::new(None)` wherever AppState is constructed.

`get_cached_full_warped_image` returns the same image the local AI masks use — check its exact return type in `src-tauri/src/lib.rs` (`Arc<DynamicImage>` per `ai_commands.rs` usage `warped_image.as_ref()`); adjust `.as_ref()`/deref accordingly.

- [ ] **Step 2: Compile + tests**

Run: `cd src-tauri && cargo check && cargo test remote_masking` — Expected: clean, 3 PASS

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/remote_masking.rs src-tauri/src/app_state.rs
git commit -m "feat: gateway client + generate/cancel/check remote mask commands"
```

---

### Task 4: Mask rasterization dispatch + command registration

**Files:**
- Modify: `src-tauri/src/mask_generation.rs:1302-1313`, `src-tauri/src/lib.rs:2248-2255`

- [ ] **Step 1: Dispatch arm**

In `mask_generation.rs`, in the `match sub_mask.mask_type.as_str()` block (line ~1257), next to the `"ai-foreground"` arm (line ~1305) add — using the SAME callee the `"ai-foreground"` arm uses (it decodes `maskDataBase64` from the parameters Value, exactly what `remote-ai` needs):
```rust
        "remote-ai" => {
            generate_ai_subject_bitmap(&sub_mask.parameters, width, height, scale, crop_offset)
        }
```
(If the `"ai-foreground"` arm calls a differently-named function or passes different args, mirror that arm verbatim — the parameters are bitmap-compatible by design.)

- [ ] **Step 2: Register commands**

In `lib.rs` `invoke_handler` list (after `ai_commands::test_ai_connector_connection,` line ~2254) add:
```rust
            remote_masking::generate_remote_ai_mask,
            remote_masking::cancel_remote_ai_mask,
            remote_masking::check_remote_mask_backend,
```

- [ ] **Step 3: Compile + commit**

Run: `cd src-tauri && cargo check` — Expected: clean

```bash
git add src-tauri/src/mask_generation.rs src-tauri/src/lib.rs
git commit -m "feat: remote-ai mask type dispatch + command registration"
```

---

### Task 5: Frontend type plumbing

**Files:**
- Modify: `src/components/panel/right/Masks.tsx` (enum line ~18, names line ~68, icons line ~96)
- Modify: `src/utils/maskUtils.ts` (createSubMask switch)
- Modify: `src/components/ui/AppProperties.tsx` (Invokes enum, line ~54)
- Modify: `src/i18n/locales/en.json`

- [ ] **Step 1: Mask enum + name + icon**

`Masks.tsx`:
- In `export enum Mask` add: `RemoteAi = 'remote-ai',`
- If a second parallel enum exists at line ~41 mirroring types, add the same entry there.
- In `formatMaskTypeName` add: `if (type === Mask.RemoteAi) return i18n.t('masks.types.remoteAi');`
- In the icon map (line ~96) add: `[Mask.RemoteAi]: Cloud,` and import `Cloud` from `lucide-react` alongside the existing icon imports (`Sparkles`, `User`, ...).

- [ ] **Step 2: createSubMask case**

`src/utils/maskUtils.ts`, in the `switch (type)`:
```typescript
    case Mask.RemoteAi:
      return {
        ...common,
        parameters: {
          maskDataBase64: null,
          grow: 0,
          feather: 0,
          mode: 'prompt',
          query: '',
          points: [],
          preset: 'subject',
          agentic: false,
        },
      };
```

- [ ] **Step 3: Invokes entries**

`AppProperties.tsx` `Invokes` enum (alphabetical placement near `GenerateAiSubjectMask`):
```typescript
  GenerateRemoteAiMask = 'generate_remote_ai_mask',
  CancelRemoteAiMask = 'cancel_remote_ai_mask',
  CheckRemoteMaskBackend = 'check_remote_mask_backend',
```

- [ ] **Step 4: i18n strings**

`src/i18n/locales/en.json` — add under the existing `masks.types` object:
```json
"remoteAi": "AI (Remote)"
```
and a new `masks.remote` object (used by Task 6):
```json
"remote": {
  "prompt": "Prompt",
  "points": "Points",
  "paint": "Paint",
  "preset": "Preset",
  "agentic": "Agentic refine",
  "generate": "Generate",
  "cancel": "Cancel",
  "queued": "Queued (position {{position}})",
  "running": "Generating…",
  "uploading": "Uploading image…",
  "found": "Found: {{labels}}",
  "alignmentWarning": "Mask generated from the original RAW; alignment is best-effort. Switch payload to TIFF for exact alignment.",
  "presets": { "subject": "Subject", "sky": "Sky", "foreground": "Foreground" }
}
```
(Other locales fall back to English keys; do not translate in this plan.)

- [ ] **Step 5: Type-check + commit**

Run: `npm run build` — Expected: type-check passes

```bash
git add src/components/panel/right/Masks.tsx src/utils/maskUtils.ts src/components/ui/AppProperties.tsx src/i18n/locales/en.json
git commit -m "feat: remote-ai mask type in frontend model"
```

---

### Task 6: useRemoteAiMasking hook + RemoteMaskControls panel

**Files:**
- Create: `src/hooks/useRemoteAiMasking.ts`
- Create: `src/components/panel/right/RemoteMaskControls.tsx`
- Modify: `src/components/panel/right/MasksPanel.tsx` (render controls when selected sub-mask type is `remote-ai`)

- [ ] **Step 1: Hook**

`src/hooks/useRemoteAiMasking.ts` (modeled on `useAiMasking.ts` — same store access and updateSubMask pattern):
```typescript
import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'react-toastify';
import { useEditorStore } from '../store/useEditorStore';
import { useEditorActions } from './useEditorActions';
import { Adjustments, MaskContainer } from '../utils/adjustments';
import { SubMask } from '../components/panel/right/Masks';
import { Invokes } from '../components/ui/AppProperties';

export interface RemoteMaskStatus {
  subMaskId: string;
  stage: 'uploading' | 'queued' | 'running' | 'done' | 'error';
  queuePosition?: number;
  progress?: number;
  detail?: string;
}

export function useRemoteAiMasking() {
  const { setAdjustments } = useEditorActions();
  const [status, setStatus] = useState<RemoteMaskStatus | null>(null);
  const [available, setAvailable] = useState<boolean | null>(null);

  useEffect(() => {
    invoke(Invokes.CheckRemoteMaskBackend)
      .then((r: any) => setAvailable(!!r?.available))
      .catch(() => setAvailable(false));
    const unlisten = listen<any>('remote-mask-status', (e) => {
      setStatus({
        subMaskId: e.payload.sub_mask_id,
        stage: e.payload.stage,
        queuePosition: e.payload.queue_position ?? undefined,
        progress: e.payload.progress ?? undefined,
        detail: e.payload.detail ?? undefined,
      });
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const updateSubMask = useCallback(
    (subMaskId: string, parameters: any) => {
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        masks: prev.masks.map((c: MaskContainer) => ({
          ...c,
          subMasks: c.subMasks.map((sm: SubMask) =>
            sm.id === subMaskId ? { ...sm, parameters: { ...sm.parameters, ...parameters } } : sm,
          ),
        })),
      }));
    },
    [setAdjustments],
  );

  const generate = useCallback(
    async (subMask: SubMask) => {
      const { selectedImage, adjustments } = useEditorStore.getState();
      if (!selectedImage?.path) return;
      const p: any = subMask.parameters || {};
      try {
        const newParams: any = await invoke(Invokes.GenerateRemoteAiMask, {
          request: {
            subMaskId: subMask.id,
            path: selectedImage.path,
            mode: p.mode || 'prompt',
            query: p.query || null,
            points: p.points?.length ? p.points : null,
            roiMaskB64: p.roiMaskB64 || null,
            preset: p.preset || null,
            agentic: p.agentic ?? null,
            rotation: adjustments.rotation,
            flipHorizontal: adjustments.flipHorizontal,
            flipVertical: adjustments.flipVertical,
            orientationSteps: adjustments.orientationSteps,
            jsAdjustments: adjustments,
          },
        });
        updateSubMask(subMask.id, newParams);
        if (newParams.labels?.length) {
          toast.info(`Found: ${newParams.labels.join(', ')}`);
        }
      } catch (err: any) {
        toast.error(String(err));
        setStatus({ subMaskId: subMask.id, stage: 'error', detail: String(err) });
      }
    },
    [updateSubMask],
  );

  const cancel = useCallback(async () => {
    try {
      await invoke(Invokes.CancelRemoteAiMask);
    } catch {
      /* best-effort */
    }
  }, []);

  return { generate, cancel, status, available };
}
```

Note the `request` key casing: the Rust struct derives `#[serde(rename_all = "camelCase")]`, and Tauri passes the `request` object through serde — so camelCase keys as written. `roiMaskB64` must map to the Rust field `roi_mask_b64` — add `#[serde(alias = "roiMaskB64")]` on that field in `RemoteMaskRequest` (rename_all camelCase produces `roiMaskB64` already; verify with one round-trip during E2E).

- [ ] **Step 2: Controls panel**

`src/components/panel/right/RemoteMaskControls.tsx`:
```tsx
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { SubMask } from './Masks';
import { useRemoteAiMasking } from '../../hooks/useRemoteAiMasking';

const MODES = ['prompt', 'points', 'paint', 'preset'] as const;
const PRESETS = ['subject', 'sky', 'foreground'] as const;

export default function RemoteMaskControls({
  subMask,
  onParametersChange,
}: {
  subMask: SubMask;
  onParametersChange: (params: any) => void;
}) {
  const { t } = useTranslation();
  const { generate, cancel, status, available } = useRemoteAiMasking();
  const p: any = subMask.parameters || {};
  const busy =
    status?.subMaskId === subMask.id &&
    (status.stage === 'uploading' || status.stage === 'queued' || status.stage === 'running');

  if (available === false) {
    return <p className="text-xs opacity-60">Remote mask backend unavailable — check Settings.</p>;
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="flex gap-1">
        {MODES.map((m) => (
          <button
            key={m}
            className={`px-2 py-1 text-xs rounded ${p.mode === m ? 'bg-accent' : 'bg-surface'}`}
            onClick={() => onParametersChange({ mode: m })}
          >
            {t(`masks.remote.${m}`)}
          </button>
        ))}
      </div>

      {p.mode === 'prompt' && (
        <>
          <input
            type="text"
            className="input text-sm"
            placeholder="the person on the left"
            value={p.query || ''}
            onChange={(e) => onParametersChange({ query: e.target.value })}
          />
          <label className="flex items-center gap-2 text-xs">
            <input
              type="checkbox"
              checked={!!p.agentic}
              onChange={(e) => onParametersChange({ agentic: e.target.checked })}
            />
            {t('masks.remote.agentic')}
          </label>
        </>
      )}

      {p.mode === 'preset' && (
        <div className="flex gap-1">
          {PRESETS.map((pr) => (
            <button
              key={pr}
              className={`px-2 py-1 text-xs rounded ${p.preset === pr ? 'bg-accent' : 'bg-surface'}`}
              onClick={() => onParametersChange({ preset: pr })}
            >
              {t(`masks.remote.presets.${pr}`)}
            </button>
          ))}
        </div>
      )}

      {p.mode === 'points' && (
        <p className="text-xs opacity-60">Click on the image to add points (Alt-click = background).</p>
      )}
      {p.mode === 'paint' && (
        <p className="text-xs opacity-60">Paint a rough region on the image, then Generate.</p>
      )}

      {!busy ? (
        <button className="btn-primary text-sm" onClick={() => generate(subMask)}>
          {t('masks.remote.generate')}
        </button>
      ) : (
        <div className="flex items-center gap-2 text-xs">
          <span>
            {status?.stage === 'queued'
              ? t('masks.remote.queued', { position: status.queuePosition ?? 0 })
              : status?.stage === 'uploading'
              ? t('masks.remote.uploading')
              : t('masks.remote.running')}
          </span>
          <button className="btn-secondary" onClick={cancel}>
            {t('masks.remote.cancel')}
          </button>
        </div>
      )}

      {p.alignment === 'best_effort' && (
        <p className="text-xs text-amber-500">{t('masks.remote.alignmentWarning')}</p>
      )}
    </div>
  );
}
```

Styling note: the `className`s above are placeholders for whatever MasksPanel's sibling controls actually use — open `MasksPanel.tsx`, find how the AI-subject controls render buttons/inputs, and reuse those exact classes/components (RapidRAW has its own UI kit; match it).

Points & paint interactions: v1 wires the panel + prompt/preset modes fully. For `points`, reuse the existing SAM click plumbing: find where MasksPanel handles clicks for `Mask.AiSubject` (the `startPoint`/`endPoint` flow in `useAiMasking.ts:129`) and, when the selected sub-mask is `remote-ai` in points mode, push clicked coords into `parameters.points` instead of invoking the local SAM. For `paint`, reuse the brush-stroke capture used by `Mask.Brush`, rasterize strokes to a PNG via an offscreen canvas at full image resolution, and store as `parameters.roiMaskB64` before calling generate. Both integrations touch only MasksPanel's existing event handlers with `if (subMask.type === Mask.RemoteAi)` branches.

- [ ] **Step 3: Mount in MasksPanel**

In `MasksPanel.tsx`, locate where per-type controls render for the selected sub-mask (the block that shows AI-subject/brush-specific settings) and add:
```tsx
{selectedSubMask?.type === Mask.RemoteAi && (
  <RemoteMaskControls
    subMask={selectedSubMask}
    onParametersChange={(params) => updateSubMask(selectedSubMask.id, { parameters: { ...selectedSubMask.parameters, ...params } })}
  />
)}
```
using the panel's existing `updateSubMask`-equivalent updater (find how sibling controls persist parameter changes and reuse it).

- [ ] **Step 4: Build + commit**

Run: `npm run build` — Expected: clean

```bash
git add src/hooks/useRemoteAiMasking.ts src/components/panel/right/RemoteMaskControls.tsx src/components/panel/right/MasksPanel.tsx
git commit -m "feat: remote AI mask UI (prompt/points/paint/preset modes)"
```

---

### Task 7: Settings UI

**Files:**
- Modify: `src/components/panel/SettingsPanel.tsx` (~line 522 where `aiConnectorAddress` state lives)

- [ ] **Step 1: Add state + inputs**

Follow the exact pattern of `aiConnectorAddress` (state at line ~522, sync effect at ~634, save handler at ~895): add `remoteMaskAddress` (text), `remoteMaskPayload` (select: TIFF / RAW), `remoteMaskIncludeRrdata` (toggle), `remoteMaskBackend` (select: SAM2 / SAM3), `remoteMaskAgenticDefault` (toggle), each read from `appSettings?.<key>` and written back through the same settings-save call the connector address uses. Place them directly below the connector-address input in the AI section, labeled:

- "Remote mask gateway (optional, defaults to AI backend address)"
- "Mask payload" — options `TIFF (exact)` / `RAW (best effort)`
- "Send edit sidecar (.rrdata) as hints"
- "Segmentation backend" — `SAM2` / `SAM3`
- "Agentic refinement by default"

- [ ] **Step 2: Build + commit**

Run: `npm run build` — Expected: clean

```bash
git add src/components/panel/SettingsPanel.tsx
git commit -m "feat: remote masking settings UI"
```

---

### Task 8: E2E checklist + run

**Files:**
- Create: `docs/superpowers/testing/remote-mask-e2e.md`

- [ ] **Step 1: Write the checklist**

`docs/superpowers/testing/remote-mask-e2e.md`:
```markdown
# Remote AI masking — manual E2E

Prereqs: rr-ai-gateway running on inferno (`systemctl --user status rr-ai-gateway`),
ComfyUI up on inferno:8188, RapidRAW settings → AI backend = inferno:5000.

For each payload mode (Settings → Mask payload = TIFF, then RAW), on one .ARW:

- [ ] Prompt mode: query "the main subject." → mask appears, hugs edges at 100% zoom
- [ ] Prompt + Agentic: gateway log shows mask_agentic (LLM/VLM iterations)
- [ ] Points mode: 1 fg click on subject → subject masked; +1 Alt-click bg refines
- [ ] Paint mode: rough scribble over subject → mask snaps to subject within region
- [ ] Preset: Subject / Sky / Foreground each return plausible masks
- [ ] Queue: submit agentic job, then a second mask → second shows queue position
- [ ] Cancel mid-run → job cancelled on gateway (log), UI returns to idle
- [ ] RAW payload shows best-effort alignment note; TIFF does not
- [ ] Mask adjustments (exposure -1) apply only inside mask; invert works
- [ ] .rrdata round-trip: saved file reopens with remote mask intact
- [ ] Stock inpaint (generative edit) still works against the same gateway
```

- [ ] **Step 2: Execute the checklist**

Run `npm run tauri dev`, work through every box against the live gateway. Fix what fails (each fix = its own commit).

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/testing/remote-mask-e2e.md
git commit -m "docs: remote masking E2E checklist"
```

---

## Self-review checklist

1. Spec coverage: sub-mask type ✓ (T4/T5), five input modes ✓ (T6; points/paint reuse existing interactions), payload TIFF/RAW + EXIF + rrdata ✓ (T3), settings ✓ (T1/T7), events + cancel ✓ (T3/T6), alignment warning ✓ (T6), upstream hygiene ✓ (new files + minimal hooks).
2. `cargo test` + `npm run build` green; E2E checklist fully ticked in both payload modes.
3. Rebase check: `git fetch upstream && git rebase upstream/main` applies with no conflicts outside the registered hook points.
