import { useT } from '@/i18n/useT'

export function PortfolioView() {
  const t = useT('vertical')

  return (
    <section className="terra-portfolio-view">
      <h3>{t('portfolio.title', 'Portfolio')}</h3>
      <p>{t('portfolio.stub', 'Holdings and backtest charts appear after Akshare data is connected.')}</p>
      <button type="button" disabled>
        {t('portfolio.backtest', 'Run backtest')}
      </button>
    </section>
  )
}

export default PortfolioView
