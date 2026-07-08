/**
 * gateway-mask-tools - panel.
 *
 * Reference implementation for the "panel" surface: shows a read-only
 * summary of the active `remote-ai` sub-mask's pipeline parameters, exposes
 * toggles that edit that sub-mask's parameters directly via setAdjustments,
 * and pings the gateway's /health endpoint on mount.
 *
 * Note on naming vs the design doc: RapidRAW's remote-mask pipeline doesn't
 * have a separate "backend" selector or an agentic_mode precise/removal
 * enum on the sub-mask itself (see src/hooks/useRemoteAiMasking.ts and
 * src/components/panel/right/RemoteMaskControls.tsx) - `agentic` is a plain
 * boolean toggle, and the closest analog to "backend" is the sub-mask's
 * `mode` (prompt/points/paint/preset/box/ellipse). This plugin surfaces what
 * actually exists rather than inventing fields the app doesn't have.
 */

const REMOTE_AI_MASK_TYPE = 'remote-ai';

function findRemoteAiSubMask(adjustments) {
  const masks = (adjustments && adjustments.masks) || [];
  for (const container of masks) {
    for (const subMask of container.subMasks || []) {
      if (subMask.type === REMOTE_AI_MASK_TYPE) return subMask;
    }
  }
  return null;
}

function updateSubMaskParameters(setAdjustments, subMaskId, changes) {
  setAdjustments((prev) => ({
    ...prev,
    masks: prev.masks.map((container) => ({
      ...container,
      subMasks: container.subMasks.map((sm) =>
        sm.id === subMaskId ? { ...sm, parameters: { ...sm.parameters, ...changes } } : sm,
      ),
    })),
  }));
}

export default function (api) {
  const { html, React } = api;

  function GatewayMaskToolsPanel(props) {
    const { adjustments, setAdjustments, api: panelApi } = props;
    const [health, setHealth] = React.useState(null);
    const [healthError, setHealthError] = React.useState(null);

    React.useEffect(() => {
      let cancelled = false;
      panelApi.gateway
        .fetch('/health')
        .then((res) => res.json())
        .then((json) => {
          if (!cancelled) setHealth(json);
        })
        .catch((err) => {
          if (!cancelled) setHealthError(err instanceof Error ? err.message : String(err));
        });
      return () => {
        cancelled = true;
      };
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);

    const subMask = findRemoteAiSubMask(adjustments);

    if (!subMask) {
      return html`
        <div className="p-4 text-sm text-text-secondary">
          Create an AI (Remote) mask first.
        </div>
      `;
    }

    const params = subMask.parameters || {};

    const onToggleMultirep = (e) =>
      updateSubMaskParameters(setAdjustments, subMask.id, { sam3Multirep: e.target.checked });
    const onToggleAgentic = (e) => updateSubMaskParameters(setAdjustments, subMask.id, { agentic: e.target.checked });

    const queueDepth = health && (health.queue_depth ?? health.queueDepth);

    return html`
      <div className="p-4 space-y-4 text-sm text-text-primary">
        <div>
          <div className="font-semibold mb-1">Active sub-mask</div>
          <div className="text-text-secondary">mode: ${params.mode || 'prompt'}</div>
          <div className="text-text-secondary">agentic: ${params.agentic ? 'on' : 'off'}</div>
          <div className="text-text-secondary">carve: ${params.carve ? 'on' : 'off'}</div>
        </div>

        <label className="flex items-center gap-2">
          <input type="checkbox" checked=${!!params.sam3Multirep} onChange=${onToggleMultirep} />
          <span>Multi-rep (sam3Multirep)</span>
        </label>

        <label className="flex items-center gap-2">
          <input type="checkbox" checked=${!!params.agentic} onChange=${onToggleAgentic} />
          <span>Agentic mode</span>
        </label>

        <div className="pt-3 border-t border-surface">
          <div className="font-semibold mb-1">Gateway health</div>
          ${healthError
            ? html`<div className="text-red-400">${healthError}</div>`
            : health
              ? html`<div className="text-text-secondary">
                  status: ${health.status || 'unknown'} · queue: ${queueDepth ?? 'n/a'}
                </div>`
              : html`<div className="text-text-secondary">checking…</div>`}
        </div>
      </div>
    `;
  }

  api.registerPanel({
    id: 'gateway-mask-tools-panel',
    title: 'Gateway Mask Tools',
    component: GatewayMaskToolsPanel,
  });
}
