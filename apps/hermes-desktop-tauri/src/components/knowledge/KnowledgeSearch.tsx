import { useState } from 'react'

import { useT } from '@/i18n/useT'

export function KnowledgeSearch() {
  const t = useT('vertical')
  const [query, setQuery] = useState('')

  return (
    <section className="terra-knowledge-search">
      <h3>{t('search.title', 'Knowledge search')}</h3>
      <input
        value={query}
        placeholder={t('search.placeholder', 'Semantic search...')}
        onChange={e => setQuery(e.target.value)}
      />
      <p className="terra-knowledge-search__hint">
        {t('search.stub', 'Embeddings index builds locally when items are imported.')}
      </p>
    </section>
  )
}

export default KnowledgeSearch
