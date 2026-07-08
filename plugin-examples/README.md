# RapidRAW plugins

RapidRAW can load small, trusted JavaScript plugins from a folder on disk and
let them add UI to two places: a section in the right-hand "Plugins" panel,
and an action in the library's right-click menu. This folder ships two
reference plugins you can install with one click and read as working
examples.

There is no sandboxing - plugins run with the same access as the rest of the
app's frontend. This is meant for your own trusted, personal plugins, not for
installing code from strangers.

## Installing the examples

Settings → Plugins → "Install example plugins" copies this folder's two
plugins into your plugins directory and reloads them immediately. "Open
plugins folder" opens that directory directly; "Reload plugins" re-scans it
and re-registers everything without a restart.

## Where plugins live

```
<app-data>/plugins/<plugin-id>/
  plugin.json     # manifest
  index.js        # ES module entry point
```

`<app-data>` is the platform-standard app data directory RapidRAW already
uses for settings/presets/cache. You can also create a plugin by hand: make a
new folder under `plugins/`, add `plugin.json` and `index.js`, then hit
"Reload plugins".

## `plugin.json`

```json
{
  "name": "My Plugin",
  "version": "1.0.0",
  "entry": "index.js",
  "surfaces": ["panel", "library"],
  "description": "Optional, shown in Settings."
}
```

- `id` is always the folder name, not a field in this file - if you write one
  anyway it's ignored.
- `surfaces` is informational for now (both `registerPanel` and
  `registerLibraryAction` are always available regardless of what you list
  here); list what your plugin actually registers so Settings' plugin list is
  accurate.
- A missing/invalid manifest doesn't crash anything - the plugin shows up in
  Settings with an error badge and simply isn't loaded.

## `index.js`

A plugin is an ES module whose default export is a function that receives
the plugin API and registers whatever it needs:

```js
export default function (api) {
  api.registerLibraryAction({
    id: 'my-action',
    title: 'Do the thing',
    onRun: async (paths) => {
      /* paths: string[] of the frozen library selection at click time */
    },
  });
}
```

A throwing plugin (at load time, or later during a panel render) never takes
the app down - it's caught, toasted, and logged; only that plugin is
affected.

### The API surface (`apiVersion: 1`)

- `api.registerPanel({ id, title, icon?, component })` - adds a collapsible
  section to the Plugins panel. `component` is a React function component
  receiving `{ adjustments, setAdjustments, selectedImagePath, appSettings, api }`
  - the same shape as RapidRAW's own adjustment-panel components.
- `api.registerLibraryAction({ id, title, icon?, onRun(paths) })` - adds an
  entry under "Plugins ▸" in the library thumbnail context menu. `onRun`
  receives the selection's paths (virtual-copy-aware, same strings the rest
  of the app uses) and may be async.
- `api.React` / `api.html` - since plugins aren't built/transpiled, there's no
  JSX. `api.html` is [`htm`](https://github.com/developit/htm) bound to
  `React.createElement`, so you write JSX-shaped markup as a tagged template
  instead: `` html`<div className="p-2">${value}</div>` ``. Use
  `className`, not `class` - this still goes through `React.createElement`.
- `api.invoke(cmd, args?)` - passthrough to Tauri's `invoke`, for calling any
  existing backend command directly.
- `api.toast(kind, message)` - `kind` is `'success' | 'error' | 'info' | 'warning'`.
- `api.presets.load()` / `api.presets.save(list)` - typed wrappers over the
  same `load_presets`/`save_presets` commands the Presets panel uses. The
  shape is an array of `{ folder: { id, name, children: Preset[] } } | { preset: Preset }`
  items; `Preset` is `{ id, name, adjustments, includeMasks?, includeCropTransform?, presetType? }`.
  See `exif-auto-preset/index.js` for a worked example of merging into an
  existing named folder.
- `api.exif.read(paths)` - wraps `read_exif_for_paths`; returns
  `{ [path]: { [ExifFieldName]: string } }`. Field names match what
  `src-tauri/src/exif_processing.rs` produces (e.g. `Make`, `LensModel`,
  `FNumber`, `ExposureTime`, `PhotographicSensitivity`) - there's no
  standardized numeric EXIF shape, so plugins parse these display strings
  themselves (see `exif-auto-preset` for the parsing helpers).
- `api.gateway.url()` / `api.gateway.fetch(path, init?)` - the remote-AI
  gateway address configured in Settings (falls back to the AI Connector
  address if the remote-mask address isn't set), and a `fetch()` wrapper
  pointed at it. `fetch` rejects if no address is configured. There is no
  bundled upload helper in v1 - if a plugin needs to upload image bytes, it
  has to build that request itself against whatever the gateway expects.

## The two bundled examples

### `exif-auto-preset` (library action)

Reads EXIF for the selected image(s) and builds one preset per image into an
"Auto (EXIF)" preset folder: an exposure nudge estimated from EV, ISO-scaled
noise reduction, and lens-correction enablement seeded from the shot's
maker/model. This is a JS port of a personal helper script
(`exif_to_rrpreset.py`) that did the same thing offline; the numeric formulas
are copied field-for-field, including how it derives "lensMaker" from the
camera body's `Make` tag rather than the lens's own manufacturer - ported
as-is rather than "fixed", since the original script's presets are meant to
be reproducible from either place. The script's content-analysis pass (scene
detection, palette, tags) depends on a local-only sibling module and is not
part of this plugin; presets here are named after the source file instead of
a scene label.

### `gateway-mask-tools` (panel)

A read-only summary of the active `remote-ai` sub-mask's pipeline parameters
(mode/agentic/carve), two toggles that edit that sub-mask's parameters
directly (`sam3Multirep` and `agentic`), and a gateway `/health` check on
mount. If there's no `remote-ai` mask in the current image's adjustments yet,
it shows a hint to create one first. This is intentionally modest - it's a
reference implementation for the panel surface, not a full mask editor (the
Masks panel already has one).

Note: the plugin surfaces what the app's remote-mask pipeline actually has
today. There's no separate "backend" selector or a precise/removal
`agentic_mode` enum on the sub-mask itself - `agentic` is a plain boolean
toggle, and the closest analog to "backend" is the sub-mask's `mode`
(prompt/points/paint/preset/box/ellipse). See
`src/hooks/useRemoteAiMasking.ts` and
`src/components/panel/right/RemoteMaskControls.tsx` if you want to extend
this further.
