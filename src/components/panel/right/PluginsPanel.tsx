import { useState } from 'react';
import { Puzzle } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useShallow } from 'zustand/react/shallow';
import Text from '../../ui/Text';
import { TextVariants } from '../../../types/typography';
import CollapsibleSection from '../../ui/CollapsibleSection';
import { useEditorStore } from '../../../store/useEditorStore';
import { useEditorActions } from '../../../hooks/useEditorActions';
import { usePluginRegistry } from '../../../plugins/registry';
import PluginPanelHost from '../../../plugins/PluginPanelHost';

/**
 * Hosts every registered plugin panel, stacked as its own collapsible
 * section (titled by the plugin's declared panel title). This is the ONE
 * entry added to the right-panel switcher for the whole plugin system - a
 * plugin adding a panel never needs a new Panel enum value or switcher icon.
 */
export default function PluginsPanel() {
  const { t } = useTranslation();
  const { setAdjustments } = useEditorActions();
  const { adjustments, selectedImage } = useEditorStore(
    useShallow((state) => ({
      adjustments: state.adjustments,
      selectedImage: state.selectedImage,
    })),
  );
  const panels = usePluginRegistry(useShallow((state) => state.panels));
  const [openIds, setOpenIds] = useState<Record<string, boolean>>({});

  const isOpen = (key: string) => openIds[key] ?? true;
  const toggle = (key: string) => setOpenIds((prev) => ({ ...prev, [key]: !isOpen(key) }));

  return (
    <div className="flex flex-col h-full">
      <div className="p-4 flex items-center gap-2 shrink-0 border-b border-surface">
        <Puzzle size={18} />
        <Text variant={TextVariants.title}>{t('editor.plugins.title')}</Text>
      </div>

      <div className="grow overflow-y-scroll p-4 flex flex-col gap-2">
        {panels.length === 0 ? (
          <Text variant={TextVariants.small} className="text-text-secondary">
            {t('editor.plugins.empty')}
          </Text>
        ) : (
          panels.map((panel) => {
            const key = `${panel.pluginId}:${panel.id}`;
            return (
              <div className="shrink-0" key={key}>
                <CollapsibleSection
                  canToggleVisibility={false}
                  isContentVisible={true}
                  isOpen={isOpen(key)}
                  onToggle={() => toggle(key)}
                  title={panel.title}
                >
                  <PluginPanelHost
                    panel={panel}
                    adjustments={adjustments}
                    setAdjustments={setAdjustments}
                    selectedImagePath={selectedImage?.path ?? null}
                  />
                </CollapsibleSection>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
