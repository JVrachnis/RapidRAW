# RapidRAW Plugin System v1 (Design)

**Date:** 2026-07-09
**Branch:** feat/remote-ai-masking (continues the fork's feature branch)
**Status:** awaiting user approval

## Purpose

Let the user extend RapidRAW without touching core code: JS/TS plugins loaded from a
local folder that can (a) add sections to the right control panel and (b) add actions
to the library operating on the current selection. Trusted execution (personal fork),
hot-reloadable, with two reference plugins proving both surfaces.

## Decisions (defaults announced 2026-07-09)

- **Runtime:** ES-module JS plugins loaded from `<app-data>/plugins/<id>/` at startup.
  The AI gateway remains the external-process extension point; plugins may call it.
- **Trust:** no sandbox v1. Manifest-scoped permissions are a later concern.
- **Reference plugins:** `gateway-mask-tools` (panel) and `exif-auto-preset` (library).

## Grounding (recon anchors)

- Panel registry: `RightPanelSwitcher.tsx:28-40` (`panelGroups`), section components map
  in `ControlsPanel.tsx:275-312`; panels receive `{adjustments, setAdjustments, ...}`.
- Library selection: `useLibraryStore.ts:32-38` (`multiSelectedPaths`); context menu
  built in `useAppContextMenus.ts:322-359` (`Option[]` — injectable).
- Presets backend exists: `load_presets`/`save_presets` (file_management.rs:2536-2550);
  EXIF: `read_exif_for_paths` (file_management.rs:198-232). CSP: `null`
  (tauri.conf.json:25) → `import(blobUrl)` works.

## Architecture

### Plugin package

```
<app-data>/plugins/<plugin-id>/
  plugin.json     # {"id","name","version","entry":"index.js","surfaces":["panel"|"library",...],"description"?}
  index.js        # ES module; default export: (api) => void — registers surfaces
  ...assets
```

### Rust additions (2 commands + settings)

- `list_plugins() -> Vec<PluginManifest>` — scan the plugins dir, parse+validate
  manifests (bad manifest → skipped with a warning entry, never a panic).
- `read_plugin_entry(plugin_id) -> String` — read the entry JS; canonicalized path
  MUST stay under the plugins dir (traversal guard). No generic fs exposure.
- `AppSettings`: `plugins_enabled: Option<bool>` (default true),
  `disabled_plugins: Option<Vec<String>>` (opt-out list; new plugins load by default).
- Plugins dir created on demand at `app_data_dir()/plugins`.

### Frontend host (`src/plugins/`)

- `loader.ts` — on startup (and via a Reload button): `list_plugins` →
  filter enabled → `read_plugin_entry` → `import(blobUrl)` → `mod.default(api)`.
  Every plugin call wrapped try/catch: a throwing plugin is disabled for the session
  with a toast; the app never crashes because of a plugin.
- `registry.ts` — zustand store: `panels: PluginPanel[]`, `libraryActions:
  PluginLibraryAction[]`; registration validates shapes.
- `api.ts` — the stable surface handed to plugins (`apiVersion: 1`):
  - `registerPanel({id, title, icon?, component})` — component: React FC receiving
    `{adjustments, setAdjustments, selectedImagePath, appSettings, api}`.
  - `registerLibraryAction({id, title, onRun(paths: string[]), icon?})`.
  - `React` + `html` (bundled `htm` bound to createElement) — plugins author UI
    without JSX/build steps.
  - `invoke(cmd, args)` — Tauri passthrough. `toast(kind, msg)`.
  - `gateway` — `{ url(), fetch(path, init), maskJob(params, {onStatus}) }` helper
    (upload-by-path via existing Rust flows is NOT reused; gateway helper does plain
    HTTP: upload file bytes fetched via a small `read_file_b64` — NO: v1 scope cut,
    `gateway.fetch` only + documented; full upload helper when a plugin needs it).
  - `presets: {load(), save(list)}`, `exif: {read(paths)}` — typed wrappers over the
    existing commands.
- `PluginPanelHost.tsx` — renders a registered panel inside an ErrorBoundary; mounts
  via one new entry appended to `panelGroups` per plugin panel (plugin group at the
  bottom of the switcher), and a case in the ControlsPanel component map.
- Library integration: `useAppContextMenus` gains a "Plugins ▸" submenu (only when
  registry has actions and selection non-empty) listing actions; `onRun` receives the
  frozen selection.
- Settings UI: a Plugins card — master toggle, per-plugin enable/disable checkboxes,
  Reload plugins, Open plugins folder (shell plugin exists).

### Reference plugins (shipped in repo `plugin-examples/`, plus a one-click
"Install example plugins" button in the settings card that copies them into the dir)

1. **gateway-mask-tools** (panel): controls for the active `remote-ai` sub-mask's
   advanced params (agentic_mode precise/removal, sam3 thresholds once exposed,
   multirep, carve) + a Generate button — edits `adjustments.masks[...].parameters`
   via `setAdjustments`, triggers the existing generate command through `api.invoke`.
2. **exif-auto-preset** (library): for selected paths, `api.exif.read` → port of
   `exif_to_rrpreset.py`'s mapping (lens maker/model → lens correction enabled, WB
   temp/tint estimate, EV-based exposure nudge, ISO-scaled noise reduction) →
   `api.presets.save` into an "Auto (EXIF)" preset folder → toast per image.

## Error handling

Loader failures per plugin are isolated (skip + toast + console detail). Panel render
errors caught by ErrorBoundary (panel shows inline "plugin crashed" card). Library
action errors toast. `apiVersion` mismatch (future) → plugin skipped with message.

## Testing

- Rust: unit tests for manifest parsing (valid/invalid/traversal attempt).
- Frontend: vitest (new minimal dev-dep) for loader filtering, registry validation,
  and the exif→preset mapping function (pure, ported with test vectors from the
  Python original). Typecheck zero-new-errors baseline discipline continues.
- Manual E2E: both reference plugins exercised in the dev app.

## Upstream hygiene

New files under `src/plugins/`, `plugin-examples/`, one Rust module
(`src-tauri/src/plugins.rs`). Hooks limited to: panelGroups append, ControlsPanel map
case, context-menu submenu, settings card, `mod plugins;` + 2 command registrations.

## Out of scope v1

Sandboxing/permissions; plugin marketplace/updates; Rust-side dynamic plugins;
custom develop-pipeline stages (GPU shader hooks); plugin-to-plugin APIs.
