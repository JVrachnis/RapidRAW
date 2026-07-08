// The stable surface handed to every plugin's default export: (api) => void.
// Everything here is scoped to the plugin that requested it (registerPanel/
// registerLibraryAction auto-tag with pluginId) and every call is safe to invoke
// from untyped plugin JS - nothing here reaches into app internals beyond the
// existing Tauri commands and the registry.
import * as React from 'react';
import htm from 'htm';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { Invokes, type AppSettings } from '../components/ui/AppProperties';
import type { UserPreset } from '../hooks/usePresets';
import { usePluginRegistry } from './registry';
import type { PluginApi, PluginLibraryAction, PluginPanel, ToastKind } from './types';

const html = htm.bind(React.createElement) as PluginApi['html'];

function normalizeGatewayAddress(address: string): string {
  const trimmed = address.trim().replace(/\/+$/, '');
  return /^https?:\/\//i.test(trimmed) ? trimmed : `http://${trimmed}`;
}

function resolveGatewayUrl(settings: AppSettings | null): string | null {
  const address = settings?.remoteMaskAddress || settings?.aiConnectorAddress;
  return address ? normalizeGatewayAddress(address) : null;
}

/**
 * Build the frozen API object passed to a single plugin's default export.
 * `getAppSettings` is a live getter (not a snapshot) so `gateway.url()` always
 * reflects the current settings even if the plugin holds onto the api object.
 */
export function makeApi(pluginId: string, getAppSettings: () => AppSettings | null): PluginApi {
  const api: PluginApi = {
    apiVersion: 1,
    React,
    html,

    registerPanel: (panel) => {
      const full: PluginPanel = { ...panel, pluginId };
      usePluginRegistry.getState().registerPanel(full);
    },

    registerLibraryAction: (action) => {
      const full: PluginLibraryAction = { ...action, pluginId };
      usePluginRegistry.getState().registerLibraryAction(full);
    },

    invoke: (cmd, args) => invoke(cmd, args),

    toast: (kind: ToastKind, message: string) => {
      toast[kind](message);
    },

    presets: {
      load: () => invoke<UserPreset[]>(Invokes.LoadPresets),
      save: (presets: UserPreset[]) => invoke<void>(Invokes.SavePresets, { presets }),
    },

    exif: {
      read: (paths: string[]) =>
        invoke<Record<string, Record<string, string>>>(Invokes.ReadExifForPaths, { paths }),
    },

    gateway: {
      url: () => resolveGatewayUrl(getAppSettings()),
      fetch: (path: string, init?: RequestInit) => {
        const base = resolveGatewayUrl(getAppSettings());
        if (!base) {
          return Promise.reject(new Error('gateway.fetch: no gateway address configured in settings'));
        }
        const suffix = path.startsWith('/') ? path : `/${path}`;
        return window.fetch(`${base}${suffix}`, init);
      },
    },
  };

  return Object.freeze(api);
}
