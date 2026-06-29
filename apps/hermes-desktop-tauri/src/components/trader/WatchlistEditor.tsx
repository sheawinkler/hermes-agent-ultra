import { useState } from 'react'

import { useT } from '@/i18n/useT'

export interface WatchlistRule {
  id: string
  symbol: string
  kind: 'pct_change' | 'volume' | 'announcement' | 'earnings'
  threshold: string
}

export function WatchlistEditor() {
  const t = useT('vertical')
  const [symbols, setSymbols] = useState<string[]>([])
  const [draft, setDraft] = useState('')

  const addSymbol = () => {
    const symbol = draft.trim().toUpperCase()
    if (!symbol || symbols.includes(symbol)) return
    setSymbols(prev => [...prev, symbol])
    setDraft('')
  }

  return (
    <section className="terra-watchlist-editor">
      <h3>{t('watchlist.title', 'Watchlist')}</h3>
      <div className="terra-watchlist-editor__add">
        <input
          value={draft}
          placeholder={t('watchlist.symbol', 'Symbol e.g. 600519')}
          onChange={e => setDraft(e.target.value)}
          onKeyDown={e => e.key === 'Enter' && addSymbol()}
        />
        <button type="button" onClick={addSymbol}>
          {t('watchlist.add', 'Add')}
        </button>
      </div>
      <ul>
        {symbols.map(symbol => (
          <li key={symbol}>{symbol}</li>
        ))}
      </ul>
      <p className="terra-watchlist-editor__hint">
        {t('watchlist.rulesHint', 'Alert rules sync via /api/schedules when backend is configured.')}
      </p>
    </section>
  )
}

export default WatchlistEditor
