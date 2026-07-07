import { useTranslation } from 'react-i18next';
import { Loader2, Send, X } from 'lucide-react';
import Switch from '../../ui/Switch';
import Input from '../../ui/Input';
import Button from '../../ui/Button';
import Text from '../../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../../types/typography';
import { RemoteMaskStatus } from '../../../hooks/useRemoteAiMasking';
import { SubMask } from './Masks';

const MODES: Array<'prompt' | 'points' | 'paint' | 'preset'> = ['prompt', 'points', 'paint', 'preset'];
const PRESETS = ['subject', 'sky', 'foreground'];

interface RemoteMaskControlsProps {
  subMask: SubMask;
  onParametersChange(changes: Record<string, any>): void;
  onGenerate(): void;
  onCancel(): void;
  status?: RemoteMaskStatus;
  onDragStateChange?: (isDragging: boolean) => void;
}

export default function RemoteMaskControls({
  subMask,
  onParametersChange,
  onGenerate,
  onCancel,
  status,
}: RemoteMaskControlsProps) {
  const { t } = useTranslation();
  const params = subMask.parameters || {};
  const mode = params.mode || 'prompt';
  const labels: string[] = Array.isArray(params.labels) ? params.labels : [];
  const isBusy = !!status;

  const statusLine = (() => {
    if (!status) return null;
    switch (status.stage) {
      case 'uploading':
        return t('masks.remote.uploading');
      case 'queued':
        return t('masks.remote.queued', { position: status.queuePosition ?? '…' });
      case 'running':
        return t('masks.remote.running');
      default:
        return null;
    }
  })();

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-4 gap-2">
        {MODES.map((m) => (
          <button
            key={m}
            className={`p-2 rounded-md text-sm font-medium transition-colors flex items-center justify-center gap-2 ${
              mode === m ? 'text-primary bg-surface' : 'bg-surface text-text-secondary hover:bg-card-active'
            }`}
            onClick={() => onParametersChange({ mode: m })}
            disabled={isBusy}
          >
            {t(('masks.remote.' + m) as any)}
          </button>
        ))}
      </div>

      {mode === 'prompt' && (
        <div className="space-y-3">
          <Input
            className="w-full"
            disabled={isBusy}
            onChange={(e: any) => onParametersChange({ query: e.target.value })}
            onKeyDown={(e: any) => {
              if (e.key === 'Enter') onGenerate();
            }}
            placeholder={t('masks.remote.prompt')}
            type="text"
            value={params.query || ''}
          />
          <Switch
            checked={!!params.agentic}
            label={t('masks.remote.agentic')}
            onChange={(v) => onParametersChange({ agentic: v })}
            disabled={isBusy}
          />
          <Switch
            checked={!!params.sam3Multirep}
            label={t('masks.remote.multirep')}
            onChange={(v) => onParametersChange({ sam3Multirep: v })}
            disabled={isBusy}
          />
        </div>
      )}

      {mode === 'preset' && (
        <div className="grid grid-cols-3 gap-2">
          {PRESETS.map((p) => (
            <button
              key={p}
              className={`p-2 rounded-md text-sm font-medium transition-colors flex items-center justify-center gap-2 ${
                params.preset === p ? 'text-primary bg-surface' : 'bg-surface text-text-secondary hover:bg-card-active'
              }`}
              onClick={() => onParametersChange({ preset: p })}
              disabled={isBusy}
            >
              {t(('masks.remote.presets.' + p) as any)}
            </button>
          ))}
        </div>
      )}

      {mode === 'points' && (
        <Text variant={TextVariants.small} color={TextColors.secondary}>
          {t('masks.remote.pointsHint')}
        </Text>
      )}

      {mode === 'paint' && (
        <Text variant={TextVariants.small} color={TextColors.secondary}>
          {t('masks.remote.paintHint')}
        </Text>
      )}

      {labels.length > 0 && (
        <Text variant={TextVariants.small} color={TextColors.secondary}>
          {t('masks.remote.found', { labels: labels.join(', ') })}
        </Text>
      )}

      {params.alignment === 'best_effort' && (
        <Text
          as="div"
          variant={TextVariants.small}
          color={TextColors.accent}
          weight={TextWeights.medium}
          className="p-3 bg-card-active rounded-md border border-surface"
        >
          {t('masks.remote.alignmentWarning')}
        </Text>
      )}

      {isBusy ? (
        <div className="flex items-center gap-2">
          <div className="flex-1 flex items-center gap-2 px-3 py-2 rounded-md bg-surface">
            <Loader2 size={16} className="animate-spin shrink-0" />
            <Text variant={TextVariants.small}>{statusLine}</Text>
          </div>
          <Button className="bg-surface" onClick={onCancel}>
            <X size={16} />
            <span>{t('masks.remote.cancel')}</span>
          </Button>
        </div>
      ) : (
        <Button className="w-full" onClick={onGenerate}>
          <Send size={16} />
          <span className="ml-2">{t('masks.remote.generate')}</span>
        </Button>
      )}
    </div>
  );
}
