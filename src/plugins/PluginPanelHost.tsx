// Renders a single registered plugin panel inside an ErrorBoundary. A plugin's
// render function is untrusted-ish (see loader.ts) - a throw during render
// must not take down the rest of the app, so it's isolated per-panel here
// rather than at some higher, shared boundary.
import { Component, useMemo, type ErrorInfo, type ReactNode } from 'react';
import { useSettingsStore } from '../store/useSettingsStore';
import { makeApi } from './api';
import type { PluginPanel, PluginPanelProps } from './types';

interface PluginPanelErrorBoundaryProps {
  title: string;
  children: ReactNode;
}

interface PluginPanelErrorBoundaryState {
  error: Error | null;
}

class PluginPanelErrorBoundary extends Component<PluginPanelErrorBoundaryProps, PluginPanelErrorBoundaryState> {
  state: PluginPanelErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): PluginPanelErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error(`[plugins] panel "${this.props.title}" crashed:`, error, info.componentStack);
  }

  render(): ReactNode {
    const { error } = this.state;
    if (error) {
      return (
        <div className="p-4 rounded-md border border-red-500/50 bg-red-900/10 text-sm">
          <p className="font-semibold text-red-400">Plugin &quot;{this.props.title}&quot; crashed</p>
          <p className="mt-1 text-text-secondary break-words">{error.message}</p>
        </div>
      );
    }
    return this.props.children;
  }
}

export interface PluginPanelHostProps {
  panel: PluginPanel;
  adjustments: PluginPanelProps['adjustments'];
  setAdjustments: PluginPanelProps['setAdjustments'];
  selectedImagePath: string | null;
}

export default function PluginPanelHost({
  panel,
  adjustments,
  setAdjustments,
  selectedImagePath,
}: PluginPanelHostProps) {
  const appSettings = useSettingsStore((state) => state.appSettings ?? null);
  const PanelComponent = panel.component;
  // Stable across renders: plugin authors may legitimately put `api` in a
  // useEffect dependency array; a fresh object per render would loop forever.
  const api = useMemo(
    () => makeApi(panel.pluginId, () => useSettingsStore.getState().appSettings ?? null),
    [panel.pluginId],
  );

  return (
    <PluginPanelErrorBoundary title={panel.title}>
      <PanelComponent
        adjustments={adjustments}
        setAdjustments={setAdjustments}
        selectedImagePath={selectedImagePath}
        appSettings={appSettings}
        api={api}
      />
    </PluginPanelErrorBoundary>
  );
}
