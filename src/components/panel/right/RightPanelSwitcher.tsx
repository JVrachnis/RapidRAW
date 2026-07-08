import { motion } from 'framer-motion';
import {
  SlidersHorizontal,
  Info,
  Crop,
  Layers,
  Paintbrush,
  SwatchBook,
  FileInput,
  Puzzle,
  type LucideIcon,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Panel } from '../../ui/AppProperties';
import { usePluginRegistry } from '../../../plugins/registry';

interface PanelOptions {
  icon: LucideIcon;
  id: Panel;
  title: string;
}

interface RightPanelSwitcherProps {
  activePanel: Panel | null;
  onPanelSelect(id: Panel): void;
  isInstantTransition: boolean;
  layout?: 'horizontal' | 'vertical';
}

const BASE_PANEL_GROUPS: Array<Array<PanelOptions>> = [
  [{ id: Panel.Metadata, icon: Info, title: 'editor.switcher.tooltips.info' }],
  [
    { id: Panel.Adjustments, icon: SlidersHorizontal, title: 'editor.switcher.tooltips.adjust' },
    { id: Panel.Crop, icon: Crop, title: 'editor.switcher.tooltips.crop' },
    { id: Panel.Masks, icon: Layers, title: 'editor.switcher.tooltips.masks' },
    { id: Panel.Ai, icon: Paintbrush, title: 'editor.switcher.tooltips.inpaint' },
  ],
  [
    { id: Panel.Presets, icon: SwatchBook, title: 'editor.switcher.tooltips.presets' },
    { id: Panel.Export, icon: FileInput, title: 'editor.switcher.tooltips.export' },
  ],
];

export default function RightPanelSwitcher({
  activePanel,
  onPanelSelect,
  isInstantTransition,
  layout = 'vertical',
}: RightPanelSwitcherProps) {
  const { t } = useTranslation();
  const isHorizontal = layout === 'horizontal';
  const hasPluginPanels = usePluginRegistry((state) => state.panels.length > 0);

  // Plugins get exactly one entry here, appended as its own group at the
  // bottom of the switcher, and only once at least one plugin panel is
  // actually registered - an empty "Plugins" icon that opens nothing would
  // just be clutter.
  const panelGroups = hasPluginPanels
    ? [...BASE_PANEL_GROUPS, [{ id: Panel.Plugins, icon: Puzzle, title: 'editor.switcher.tooltips.plugins' }]]
    : BASE_PANEL_GROUPS;

  return (
    <div className={isHorizontal ? 'flex items-center overflow-x-auto p-1 gap-1' : 'flex flex-col p-1 gap-1 h-full'}>
      {panelGroups.map((group, groupIndex) => (
        <div key={groupIndex} className={isHorizontal ? 'flex items-center gap-1' : 'flex flex-col gap-1'}>
          {groupIndex > 0 && (
            <div
              className={isHorizontal ? 'w-px h-6 bg-surface self-stretch my-auto' : 'w-6 h-px bg-surface self-center'}
            />
          )}
          {group.map(({ id, icon: Icon, title }) => (
            <button
              className={`relative rounded-md transition-colors duration-200 ${isHorizontal ? 'p-2 shrink-0' : 'p-2'} ${
                activePanel === id
                  ? 'text-text-primary'
                  : 'text-text-secondary hover:bg-surface hover:text-text-primary'
              }`}
              key={id}
              onClick={() => onPanelSelect(id)}
              data-tooltip={t(title)}
            >
              {activePanel === id && (
                <motion.div
                  layoutId="active-panel-indicator"
                  className="absolute inset-0 bg-surface rounded-md"
                  transition={isInstantTransition ? { duration: 0 } : { type: 'spring', bounce: 0.2, duration: 0.4 }}
                />
              )}
              <Icon size={20} className="relative z-10" />
            </button>
          ))}
        </div>
      ))}
    </div>
  );
}
