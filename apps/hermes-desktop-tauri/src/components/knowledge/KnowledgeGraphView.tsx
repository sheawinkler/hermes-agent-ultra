import { useT } from '@/i18n/useT'

export function KnowledgeGraphView() {
  const t = useT('vertical')

  return (
    <section className="terra-knowledge-graph">
      <h3>{t('graph.title', 'Knowledge graph')}</h3>
      <div className="terra-knowledge-graph__canvas" aria-hidden>
        <p>{t('graph.stub', 'Graph visualization loads when topics and edges are available.')}</p>
      </div>
    </section>
  )
}

export default KnowledgeGraphView
