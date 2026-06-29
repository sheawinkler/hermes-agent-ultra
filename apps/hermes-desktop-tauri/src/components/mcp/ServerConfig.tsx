import { useT } from '@/i18n/useT'

interface ServerConfigProps {
  serverId: string
  env?: Record<string, string>
  enabled?: boolean
  onSave?: (env: Record<string, string>, enabled: boolean) => void
}

export function ServerConfig({ serverId, env = {}, enabled = true, onSave }: ServerConfigProps) {
  const t = useT('mcp')

  return (
    <section className="terra-mcp-config" data-server-id={serverId}>
      <h3>{t('config.title', 'Server configuration')}</h3>
      <label>
        <input type="checkbox" defaultChecked={enabled} />
        {t('config.enabled', 'Enabled')}
      </label>
      <textarea
        defaultValue={JSON.stringify(env, null, 2)}
        rows={6}
        aria-label={t('config.env', 'Environment variables')}
      />
      <button type="button" onClick={() => onSave?.(env, enabled)}>
        {t('config.save', 'Save')}
      </button>
    </section>
  )
}

export default ServerConfig
