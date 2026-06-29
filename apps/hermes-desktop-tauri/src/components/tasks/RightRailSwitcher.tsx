export type RightRailMode = 'minimap' | 'outline'

interface RightRailSwitcherProps {
  mode: RightRailMode
  onChange: (mode: RightRailMode) => void
}

export function RightRailSwitcher({ mode, onChange }: RightRailSwitcherProps) {
  return (
    <div className="terra-right-rail-switcher" role="tablist">
      <button type="button" role="tab" aria-selected={mode === 'minimap'} onClick={() => onChange('minimap')}>
        Minimap
      </button>
      <button type="button" role="tab" aria-selected={mode === 'outline'} onClick={() => onChange('outline')}>
        Outline
      </button>
    </div>
  )
}

export default RightRailSwitcher
