// Shared types for the plugin system (see
// docs/superpowers/specs/2026-07-09-plugin-system-design.md). Kept dependency-free
// of api.ts/registry.ts so nothing here creates a circular import - this file only
// describes shapes, it never implements them.
import type * as ReactNS from 'react';
import type { Adjustments } from '../utils/adjustments';
import type { AppSettings } from '../components/ui/AppProperties';
import type { UserPreset } from '../hooks/usePresets';

/** Mirrors the Rust `PluginManifest` (src-tauri/src/plugins.rs), camelCase over IPC. */
export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  entry: string;
  surfaces: string[];
  description?: string;
  /** Set by the backend when plugin.json is missing/malformed/incomplete. */
  error?: string;
}

export interface PluginPanelProps {
  adjustments: Adjustments;
  setAdjustments: (value: Partial<Adjustments> | ((prev: Adjustments) => Adjustments)) => void;
  selectedImagePath: string | null;
  appSettings: AppSettings | null;
  api: PluginApi;
}

export interface PluginPanel {
  id: string;
  pluginId: string;
  title: string;
  icon?: string;
  component: ReactNS.FC<PluginPanelProps>;
}

export interface PluginLibraryAction {
  id: string;
  pluginId: string;
  title: string;
  icon?: string;
  onRun: (paths: string[]) => void | Promise<void>;
}

export interface PluginFailure {
  pluginId: string;
  message: string;
}

export type ToastKind = 'success' | 'error' | 'info' | 'warning';

/** The stable, versioned surface handed to every plugin's default export. */
export interface PluginApi {
  apiVersion: 1;
  React: typeof ReactNS;
  /** `htm` bound to `React.createElement` - JSX-less templated UI. */
  html: (strings: TemplateStringsArray, ...values: unknown[]) => unknown;
  registerPanel: (panel: Omit<PluginPanel, 'pluginId'>) => void;
  registerLibraryAction: (action: Omit<PluginLibraryAction, 'pluginId'>) => void;
  invoke: <T = unknown>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  toast: (kind: ToastKind, message: string) => void;
  presets: {
    load: () => Promise<UserPreset[]>;
    save: (presets: UserPreset[]) => Promise<void>;
  };
  exif: {
    read: (paths: string[]) => Promise<Record<string, Record<string, string>>>;
  };
  gateway: {
    /** Resolved gateway base URL (remoteMaskAddress || aiConnectorAddress), or null if unset. */
    url: () => string | null;
    fetch: (path: string, init?: RequestInit) => Promise<Response>;
  };
}
