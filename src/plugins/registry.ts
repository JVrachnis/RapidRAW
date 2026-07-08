// Zustand store holding everything plugins have registered so far. Registration
// validates shape up front (throws on a malformed panel/action) so a broken plugin
// fails loudly at load time (caught by loader.ts's try/catch) rather than blowing up
// later deep inside a render.
import { create } from 'zustand';
import type { PluginFailure, PluginLibraryAction, PluginPanel } from './types';

interface PluginRegistryState {
  panels: PluginPanel[];
  libraryActions: PluginLibraryAction[];
  failures: PluginFailure[];
  registerPanel: (panel: PluginPanel) => void;
  registerLibraryAction: (action: PluginLibraryAction) => void;
  recordFailure: (failure: PluginFailure) => void;
  clearRegistry: () => void;
}

function assertString(value: unknown, field: string, context: string): asserts value is string {
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`${context}: missing or invalid "${field}"`);
  }
}

function validatePanel(panel: PluginPanel): void {
  if (!panel || typeof panel !== 'object') {
    throw new Error('registerPanel: panel must be an object');
  }
  assertString(panel.id, 'id', 'registerPanel');
  assertString(panel.pluginId, 'pluginId', 'registerPanel');
  assertString(panel.title, 'title', 'registerPanel');
  if (typeof panel.component !== 'function') {
    throw new Error('registerPanel: "component" must be a function (React FC)');
  }
}

function validateLibraryAction(action: PluginLibraryAction): void {
  if (!action || typeof action !== 'object') {
    throw new Error('registerLibraryAction: action must be an object');
  }
  assertString(action.id, 'id', 'registerLibraryAction');
  assertString(action.pluginId, 'pluginId', 'registerLibraryAction');
  assertString(action.title, 'title', 'registerLibraryAction');
  if (typeof action.onRun !== 'function') {
    throw new Error('registerLibraryAction: "onRun" must be a function');
  }
}

export const usePluginRegistry = create<PluginRegistryState>((set) => ({
  panels: [],
  libraryActions: [],
  failures: [],

  registerPanel: (panel) => {
    validatePanel(panel);
    set((state) => ({
      panels: [...state.panels.filter((p) => !(p.pluginId === panel.pluginId && p.id === panel.id)), panel],
    }));
  },

  registerLibraryAction: (action) => {
    validateLibraryAction(action);
    set((state) => ({
      libraryActions: [
        ...state.libraryActions.filter((a) => !(a.pluginId === action.pluginId && a.id === action.id)),
        action,
      ],
    }));
  },

  recordFailure: (failure) => set((state) => ({ failures: [...state.failures, failure] })),

  clearRegistry: () => set({ panels: [], libraryActions: [], failures: [] }),
}));
