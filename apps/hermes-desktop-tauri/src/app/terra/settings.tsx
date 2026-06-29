import { useState } from 'react'

import { AccountSwitcher } from '@/components/account/AccountSwitcher'
import { ToolBudgetPanel } from '@/components/billing/ToolBudgetPanel'
import { ScheduleEditor } from '@/components/schedules/ScheduleEditor'
import { ScheduleList } from '@/components/schedules/ScheduleList'
import { AkshareDataSourceSettings } from '@/components/settings/AkshareDataSourceSettings'
import { OllamaSettings } from '@/components/settings/OllamaSettings'
import { ProviderSelector } from '@/components/settings/ProviderSelector'
import { PortfolioView } from '@/components/trader/PortfolioView'
import { WatchlistEditor } from '@/components/trader/WatchlistEditor'
import { KnowledgeGraphView } from '@/components/knowledge/KnowledgeGraphView'
import { KnowledgeSearch } from '@/components/knowledge/KnowledgeSearch'
import { ServerList } from '@/components/mcp/ServerList'
import { useT } from '@/i18n/useT'

export default function TerraSettings() {
  const t = useT('settings')
  const [localProbe, setLocalProbe] = useState(false)

  return (
    <section className="terra-settings">
      <h2>{t('title', 'Settings')}</h2>
      <AccountSwitcher />
      <ProviderSelector />
      <ToolBudgetPanel />
      <AkshareDataSourceSettings
        localAvailable={localProbe}
        onModeChange={() => {
          void fetch('/api/datasources/akshare/probe-local')
            .then(res => setLocalProbe(res.ok))
            .catch(() => setLocalProbe(false))
        }}
      />
      <OllamaSettings />
      <WatchlistEditor />
      <PortfolioView />
      <KnowledgeSearch />
      <KnowledgeGraphView />
      <ServerList />
      <ScheduleList />
      <ScheduleEditor />
    </section>
  )
}
