import { useT } from '@/i18n/useT'

interface McpServer {
  id: string
  name: string
  enabled: boolean
}

interface ServerListProps {
  servers?: McpServer[]
  onToggle?: (id: string, enabled: boolean) => void
}

export function ServerList({ servers = [], onToggle }: ServerListProps) {
  const t = useT('mcp')

  return (
    <section className="terra-mcp-list">
      <h3>{t('list.title', 'MCP servers')}</h3>
      <ul>
        {servers.length === 0 ? (
          <li>{t('list.empty', 'No MCP servers installed.')}</li>
        ) : (
          servers.map(server => (
            <li key={server.id}>
              <span>{server.name}</span>
              <button type="button" onClick={() => onToggle?.(server.id, !server.enabled)}>
                {server.enabled ? t('list.disable', 'Disable') : t('list.enable', 'Enable')}
              </button>
            </li>
          ))
        )}
      </ul>
    </section>
  )
}

export default ServerList
