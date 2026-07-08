// Plugin loader: list_plugins -> filter (master switch + per-plugin disable) ->
// read_plugin_entry -> import a blob URL -> call the plugin's default export with a
// scoped api. Every step for every plugin is isolated: one plugin throwing never
// stops another from loading, and never crashes the app - it's recorded as a
// failure (toasted + logged) and skipped.
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { Invokes, type AppSettings } from '../components/ui/AppProperties';
import { makeApi } from './api';
import { usePluginRegistry } from './registry';
import type { PluginManifest } from './types';

async function loadOnePlugin(manifest: PluginManifest, getAppSettings: () => AppSettings | null): Promise<void> {
  let objectUrl: string | null = null;

  try {
    const source = await invoke<string>(Invokes.ReadPluginEntry, { pluginId: manifest.id });
    const blob = new Blob([source], { type: 'text/javascript' });
    objectUrl = URL.createObjectURL(blob);

    // Trusted, personal-fork execution - no sandboxing in v1 (see design doc,
    // "Out of scope v1").
    const mod: { default?: (api: ReturnType<typeof makeApi>) => unknown } = await import(
      /* @vite-ignore */ objectUrl
    );

    if (typeof mod?.default !== 'function') {
      throw new Error(`plugin "${manifest.id}" has no default export function`);
    }

    const api = makeApi(manifest.id, getAppSettings);
    await mod.default(api);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    usePluginRegistry.getState().recordFailure({ pluginId: manifest.id, message });
    toast.error(`Plugin "${manifest.name || manifest.id}" failed to load: ${message}`);
    console.error(`[plugins] failed to load "${manifest.id}":`, err);
  } finally {
    if (objectUrl) URL.revokeObjectURL(objectUrl);
  }
}

/**
 * Load every enabled plugin found under `<app-data>/plugins/`. Never throws -
 * failures are recorded on the registry and toasted, one at a time, so a
 * single bad plugin can't take others down with it.
 */
export async function loadPlugins(
  appSettings: AppSettings | null,
  getAppSettings: () => AppSettings | null,
): Promise<void> {
  if (appSettings?.pluginsEnabled === false) {
    return;
  }

  let manifests: PluginManifest[] = [];
  try {
    manifests = await invoke<PluginManifest[]>(Invokes.ListPlugins);
  } catch (err) {
    console.error('[plugins] failed to list plugins:', err);
    return;
  }

  const disabled = new Set(appSettings?.disabledPlugins ?? []);

  for (const manifest of manifests) {
    if (manifest.error) {
      usePluginRegistry.getState().recordFailure({ pluginId: manifest.id, message: manifest.error });
      continue;
    }

    if (disabled.has(manifest.id)) {
      continue;
    }

    // Sequential on purpose: keeps load order predictable and per-plugin
    // errors cleanly attributable, at the cost of some parallelism we don't
    // need for a handful of local plugins.
    await loadOnePlugin(manifest, getAppSettings);
  }
}

/** Clears everything plugins have registered, then loads them all again. */
export async function reloadPlugins(
  appSettings: AppSettings | null,
  getAppSettings: () => AppSettings | null,
): Promise<void> {
  usePluginRegistry.getState().clearRegistry();
  await loadPlugins(appSettings, getAppSettings);
}
