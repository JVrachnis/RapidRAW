# RapidRAW Fork — Remote AI Masking via rr-ai-gateway (Design)

**Date:** 2026-07-07
**Repo:** fork of CyberTimon/RapidRAW (JVrachnis/RapidRAW), branch `feat/remote-ai-masking`
**Companion spec:** `rr-ai-gateway/docs/superpowers/specs/2026-07-07-ai-gateway-design.md`
**Status:** approved design, pre-implementation

## Purpose

Add a new mask source to RapidRAW: instead of only the small bundled ONNX
models (U2NetP foreground, SAM subject, sky, depth), send the image — with EXIF
and optionally the `.rrdata` sidecar — to the rr-ai-gateway on inferno, run
state-of-the-art segmentation there (GroundedSAM + SAM2/SAM3 + BiRefNet +
ViTMatte, optional agentic LLM/VLM loop), and bring the resulting alpha back as
a regular RapidRAW mask. Everything downstream of the mask bitmap (blend modes,
invert, opacity, per-mask adjustments) is reused unchanged.

## Goals

- New sub-mask type `remoteAiMask` that behaves exactly like the existing AI
  mask types once its bitmap is present.
- Five ways to drive it: text prompt, click points, painted ROI, one-shot
  presets, and an agentic toggle applying to prompt mode.
- Payload configurable per the gateway contract: linear 16-bit TIFF of the
  editor's own pixels (default, alignment-exact) or the original RAW file
  (best-effort alignment), always with EXIF, optionally with rrdata.
- Non-blocking UX: masking runs as a job; the editor stays usable; progress and
  queue position surfaced; jobs cancellable.

## Non-Goals

- Any gateway-side logic (separate spec/repo).
- Replacing the local ONNX mask tools — they remain for offline/quick use.
- Using remote masks for the generative-inpaint flow (existing flow untouched).

## Current-code anchors

- Sub-mask model & rasterization: `src-tauri/src/mask_generation.rs`
  (`SubMask { mask_type, parameters }`, `generate_mask_bitmap`). Existing AI
  masks store a full-res grayscale PNG in `parameters.mask_data_base64` plus
  the rotation/flip/orientation snapshot they were generated under.
- Local AI mask commands: `src-tauri/src/ai_commands.rs`
  (`generate_ai_foreground_mask` etc.) — they run on
  `get_cached_full_warped_image(...)`, the full-resolution warped image.
- Connector client: `src-tauri/src/ai_connector.rs` (reqwest, bearer token,
  `/upload_source` + `/inpaint`).
- Settings: `src-tauri/src/app_settings.rs` (existing self-hosted backend
  address + token fields).
- Frontend masks UI: `src/components/panel/right/Masks*` (mask type list, SAM
  click interaction, brush tool for painted sub-masks).

## Design

### 1. Sub-mask type

`remoteAiMask` sub-mask with parameters:

```jsonc
{
  "maskDataBase64": "data:image/png;base64,…",  // grayscale alpha, like existing AI masks
  "rotation": 0.0, "flipHorizontal": false,      // orientation snapshot, same fields
  "flipVertical": false, "orientationSteps": 0,  //   as existing AI mask params
  // request memory (for display + re-run):
  "mode": "prompt" | "points" | "paint" | "preset",
  "query": "…", "points": [[x,y,1]], "preset": "subject",
  "agentic": false, "backend": "sam2",
  "alignment": "exact" | "bestEffort",
  "labels": ["person"]                            // what the detector reported
}
```

Rasterization: reuse the exact bitmap-decode path of the existing AI mask types
in `mask_generation.rs` (decode PNG, orientation-correct, scale to target).
No new rasterization math.

### 2. Rust module `remote_masking.rs`

New module owning the gateway conversation. Tauri commands:

- `generate_remote_ai_mask(request, state, app_handle)` — the one workhorse:
  1. Resolve payload: TIFF mode exports the full-resolution warped image (the
     same `get_cached_full_warped_image` the local AI masks use) as linear
     16-bit TIFF; RAW mode reads the original file bytes from disk.
  2. Gather EXIF (existing `exif_processing` output for the file) and, when the
     setting is on, the `.rrdata` sidecar JSON verbatim.
  3. `POST /sources` (content-hash dedup makes repeats cheap), then
     `POST /jobs/mask`, then poll `GET /jobs/{id}` (500 ms → 2 s backoff),
     emitting `remote-mask-status` Tauri events (queued/position/running/done).
  4. On success return the parameters object above; the frontend stores it in
     the sub-mask, identical to how local AI mask results are stored.
- `cancel_remote_ai_mask(job_id)` — `DELETE /jobs/{id}`.
- `check_remote_mask_backend()` — `GET /capabilities` + `/health`; powers
  settings-page status and hides the tool when the gateway lacks `mask`.

Client code reuses `ai_connector.rs` conventions (reqwest client, bearer token,
base URL from settings) — extracted into a small shared helper rather than
duplicated.

TIFF export detail: the warped image is held as RGBA; encode R,G,B as 16-bit
per channel linear TIFF (image crate `tiff` encoder). If the in-memory pipeline
for the image is 8-bit at that stage, encode what exists — the gateway treats
TIFF generically; bit depth is an optimization, not a contract.

Coordinate contract: `points` and the painted ROI are expressed in
full-resolution warped-image pixel coordinates — the same space as the uploaded
TIFF, so the gateway needs no client-specific transforms. (RAW mode with
points/paint also uses TIFF-space coordinates; the gateway reconciles via
rrdata, response flags `bestEffort`.)

### 3. Settings

Added to `app_settings.rs` (all optional with defaults, serialized like the
existing connector fields):

- `remoteMaskAddress`: optional string; when empty (default) the existing
  self-hosted backend address is used — one gateway serves both inpaint and
  masks. Setting it points masking at a different host (split deployments).
- `remoteMaskPayload`: `"tiff"` (default) | `"raw"`
- `remoteMaskIncludeRrdata`: bool (default true)
- `remoteMaskBackend`: `"sam2"` (default) | `"sam3"`
- `remoteMaskAgenticDefault`: bool (default false)

### 4. Frontend

One new entry in the mask-type list: **“AI (Remote)”**, enabled when
`check_remote_mask_backend()` reports the capability. Its panel:

- Mode selector (segmented control): Prompt / Points / Paint / Preset.
  - **Prompt**: text field + Generate button + “Agentic” toggle.
  - **Points**: reuses the existing SAM click interaction to collect fg/bg
    points, but routes to the remote command instead of the local decoder.
  - **Paint**: reuses the brush sub-mask interaction; on Generate, the painted
    strokes are rasterized to a rough ROI PNG (existing brush rasterization)
    and sent as `roi_mask_b64`.
  - **Preset**: buttons — Subject / Sky / Foreground.
- Status line during a job: queue position → running → elapsed; Cancel button.
  Driven by `remote-mask-status` events.
- On completion: store returned parameters in the sub-mask; show returned
  `labels` as a small caption (“found: person, bicycle”); if
  `alignment: "bestEffort"`, show a subtle warning icon with a tooltip
  suggesting TIFF mode.
- Errors surface as the standard toast + the sub-mask stays editable/retryable.

### 5. Error handling

- Gateway unreachable → toast with settings hint; tool stays visible but
  disabled until next `check_remote_mask_backend()` success.
- Job error → toast with `error.kind`-specific message (`comfyui_down` gets
  “ComfyUI is not running on the backend”).
- `410 Gone` on source → transparent re-upload once, then retry the job.
- App closed mid-job → no persistence needed; jobs are cheap to re-request
  (source cache makes the re-upload free).

### 6. Testing

- **Rust unit**: TIFF export round-trip (dimensions, channel order); EXIF +
  rrdata gathering (fixture sidecar); `remoteAiMask` serde round-trip;
  poll/backoff logic against a mocked HTTP server (wiremock/httpmock).
- **Frontend**: mode-panel state machine (vitest) — mode switches, event-driven
  status transitions, cancel.
- **Manual E2E checklist** (with gateway live on inferno): one .ARW through
  each of the five modes in both payload modes; verify mask hugs subject edges
  at 100% zoom; verify agentic toggle produces the LLM/VLM loop on the gateway
  logs; verify cancel mid-job; verify stock inpaint flow still works against
  the same gateway address.

## Upstream hygiene

All new Rust code lives in `remote_masking.rs` plus minimal registration hooks
(`lib.rs` command registration, one arm in mask rasterization dispatch, settings
struct fields). UI additions are new components plus one entry in the mask-type
list. Goal: the fork rebases onto upstream RapidRAW releases with near-zero
conflicts.
