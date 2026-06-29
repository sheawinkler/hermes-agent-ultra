import { useT } from '@/i18n/useT'

export type RightRailMode = 'minimap' | 'outline' | 'branch'

interface RightRailSwitcherProps {
  mode: RightRailMode
  onChange: (mode: RightRailMode) => void
  showBranch?: boolean
}

export function RightRailSwitcher({ mode, onChange, showBranch = false }: RightRailSwitcherProps) {
  const t = useT('task')
  return (
    <div className="terra-right-rail-switcher" role="tablist">
      <button type="button" role="tab" aria-selected={mode === 'minimap'} onClick={() => onChange('minimap')}>
        {t('minimap', 'Minimap')}
      </button>
      <button type="button" role="tab" aria-selected={mode === 'outline'} onClick={() => onChange('outline')}>
        {t('outline', 'Outline')}
      </button>
      {showBranch ? (
        <button type="button" role="tab" aria-selected={mode === 'branch'} onClick={() => onChange('branch')}>
          {t('branch', 'Branch')}
        </button>
      ) : null}
    </div>
  )
}

export default RightRailSwitcher
