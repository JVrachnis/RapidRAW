/**
 * exif-auto-preset - library action.
 *
 * For every selected image: reads EXIF via api.exif.read(), derives a preset
 * (lens-correction maker/model, an exposure nudge estimated from EV, and
 * ISO-scaled noise reduction), and appends the result into an "Auto (EXIF)"
 * preset folder via api.presets.load()/save().
 *
 * This is a faithful port of preset_from_exif() from ~/comfy/exif_to_rrpreset.py
 * (a personal helper script that seeds RapidRAW presets from a shot's real
 * EXIF), adapted to the exif field names RapidRAW's own `read_exif_for_paths`
 * command actually returns (see src-tauri/src/exif_processing.rs). The
 * Python original also folds in a content-analysis pass (scene/palette/tags)
 * to build its preset name; that step depends on a local-only sibling module
 * this plugin has no access to, so it's intentionally NOT ported - presets
 * here are named after the source file instead of a scene label.
 */

const AUTO_EXIF_FOLDER_NAME = 'Auto (EXIF)';

function parseFNumber(exif) {
  const raw = exif['FNumber'] || exif['ApertureValue'];
  const match = raw && /f\/([\d.]+)/.exec(raw);
  return match ? parseFloat(match[1]) : 2.8;
}

function parseExposureSeconds(exif) {
  const raw = exif['ExposureTime'] || exif['ShutterSpeedValue'];
  if (raw) {
    const frac = /^([\d.]+)\s*\/\s*([\d.]+)/.exec(raw);
    if (frac) {
      const denom = parseFloat(frac[2]);
      return denom !== 0 ? parseFloat(frac[1]) / denom : 1 / 250;
    }
    const seconds = /^([\d.]+)/.exec(raw);
    if (seconds) return parseFloat(seconds[1]);
  }
  return 1 / 250;
}

function parseIso(exif) {
  const raw =
    exif['PhotographicSensitivity'] ||
    exif['ISOSpeed'] ||
    exif['ISOSpeedRatings'] ||
    exif['RecommendedExposureIndex'];
  const n = raw ? parseInt(raw, 10) : NaN;
  return Number.isFinite(n) && n > 0 ? n : 200;
}

// A ColorTemperature field isn't part of standard EXIF and RapidRAW's own
// EXIF reader doesn't synthesize one - the Python original always leaves
// temperature/tint neutral (0, 0) too ("RapidRAW WB picker refines"). If a
// gateway or sidecar ever starts reporting one under this key, pick it up;
// otherwise stay neutral rather than guessing.
function parseTemperatureDelta(exif) {
  const raw = exif['ColorTemperature'];
  const kelvin = raw ? parseFloat(raw) : NaN;
  if (!Number.isFinite(kelvin) || kelvin <= 0) return 0;
  // RapidRAW's `temperature` adjustment is a signed delta around a neutral
  // baseline, not an absolute Kelvin value - 5500K (daylight) is a reasonable
  // "no shift" reference point for a rough estimate.
  return Math.round(kelvin - 5500);
}

/** Port of preset_from_exif(ex, name) from exif_to_rrpreset.py. */
function presetFromExif(exif, name) {
  const iso = parseIso(exif);
  const fnum = parseFNumber(exif);
  const exposureSeconds = parseExposureSeconds(exif);
  const make = exif['Make'] || '';
  const lensModel = exif['LensModel'] || '';

  let ev = 0;
  try {
    ev = Math.log2((fnum * fnum) / exposureSeconds) - Math.log2(Math.max(iso, 50) / 100);
    if (!Number.isFinite(ev)) ev = 0;
  } catch (_err) {
    ev = 0;
  }

  const brightness = Math.round(Math.max(-1, Math.min(1, (12 - ev) * 0.06)) * 100) / 100;
  // Python's `int(...)` truncates toward zero (not round-to-nearest) - match
  // that exactly rather than Math.round, since it changes the result for any
  // ISO that isn't an exact power-of-4 multiple of 100.
  const noiseReduction = Math.max(0, Math.min(80, Math.trunc(Math.log2(Math.max(iso, 100) / 100) * 14)));

  const adjustments = {
    brightness,
    temperature: parseTemperatureDelta(exif),
    tint: 0,
    colorNoiseReduction: noiseReduction,
    luminanceNoiseReduction: Math.trunc(noiseReduction * 0.6),
    clarity: 6,
    // Lensfun auto-correction seeded from EXIF, matching the Python original
    // field-for-field (it stashes the camera Make under "lensMaker", not the
    // lens's own manufacturer - RapidRAW's lensfun lookup keys off whatever
    // strings are here, so this is ported as-is rather than "corrected").
    lensMaker: make || null,
    lensModel: lensModel || null,
    lensDistortionEnabled: Boolean(make),
    lensDistortionAmount: 100,
    lensTcaEnabled: Boolean(make),
    lensTcaAmount: 100,
    lensVignetteEnabled: Boolean(make),
    lensVignetteAmount: 100,
  };

  return { id: crypto.randomUUID(), name, adjustments };
}

function fileLabel(path) {
  const withoutQuery = path.split('?')[0];
  const base = withoutQuery.split(/[\\/]/).pop() || withoutQuery;
  const dot = base.lastIndexOf('.');
  return dot > 0 ? base.slice(0, dot) : base;
}

function mergeIntoAutoExifFolder(existingPresets, newPresets) {
  let found = false;
  const merged = existingPresets.map((item) => {
    if (item.folder && item.folder.name === AUTO_EXIF_FOLDER_NAME) {
      found = true;
      return { folder: { ...item.folder, children: [...item.folder.children, ...newPresets] } };
    }
    return item;
  });

  if (found) return merged;

  return [
    ...existingPresets,
    { folder: { id: crypto.randomUUID(), name: AUTO_EXIF_FOLDER_NAME, children: newPresets } },
  ];
}

export default function (api) {
  api.registerLibraryAction({
    id: 'auto-preset-from-exif',
    title: 'Auto preset from EXIF',
    onRun: async (paths) => {
      if (!paths || paths.length === 0) return;

      try {
        const exifByPath = await api.exif.read(paths);

        const newPresets = paths
          .filter((path) => exifByPath[path])
          .map((path) => presetFromExif(exifByPath[path], fileLabel(path)));

        if (newPresets.length === 0) {
          api.toast('warning', 'No EXIF data found for the selected image(s).');
          return;
        }

        const existingPresets = await api.presets.load();
        const updatedPresets = mergeIntoAutoExifFolder(existingPresets, newPresets);
        await api.presets.save(updatedPresets);

        api.toast(
          'success',
          `Added ${newPresets.length} preset${newPresets.length === 1 ? '' : 's'} to "${AUTO_EXIF_FOLDER_NAME}".`,
        );
      } catch (err) {
        api.toast('error', `Auto preset from EXIF failed: ${err instanceof Error ? err.message : err}`);
      }
    },
  });
}
