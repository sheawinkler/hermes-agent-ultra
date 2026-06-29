import { useState } from 'react'

import { AkshareDataSourceSettings } from '@/components/settings/AkshareDataSourceSettings'
import { useT } from '@/i18n/useT'

export default function TerraSettings() {
  const t = useT('settings')
  const [localProbe, setLocalProbe] = useState(false)

  return (
    <section className="terra-settings">
      <h2>{t('title', 'Settings')}</h2>
      <AkshareDataSourceSettings
        localAvailable={localProbe}
        onModeChange={() => {
          void fetch('/api/datasources/akshare/probe-local').then(res => setLocalProbe(res.ok)).catch(() => setLocalProbe(false))
        }}
      />
    </section>
  )
}
