import { useState } from 'react'

import { useT } from '@/i18n/useT'

export function ServerInstall() {
  const t = useT('mcp')
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<Array<{ id: string; name: string }>>([])

  const search = async () => {
    const res = await fetch(`/api/mcp/marketplace?q=${encodeURIComponent(query)}`)
    if (!res.ok) return
    const body = (await res.json()) as { servers?: Array<{ id: string; name: string }> }
    setResults(body.servers ?? [])
  }

  return (
    <section className="terra-mcp-install">
      <h3>{t('install.title', 'Install MCP server')}</h3>
      <input value={query} onChange={e => setQuery(e.target.value)} placeholder={t('install.search', 'Search marketplace')} />
      <button type="button" onClick={() => void search()}>
        {t('install.searchBtn', 'Search')}
      </button>
      <ul>
        {results.map(item => (
          <li key={item.id}>
            {item.name}
            <button type="button">{t('install.add', 'Install')}</button>
          </li>
        ))}
      </ul>
    </section>
  )
}

export default ServerInstall
