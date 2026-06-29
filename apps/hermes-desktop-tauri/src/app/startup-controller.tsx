import { useStore } from '@nanostores/react'
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'

import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import type {
  DesktopBootstrapState,
  DesktopConnectionProbeResult,
  DesktopUpdateSourceConfig
} from '@/global'
import { useI18n } from '@/i18n'
import { AlertCircle, ArrowUpRight, Check, CheckCircle2, ChevronLeft, Globe, Loader2, Monitor, RefreshCw, Settings2, Sparkles } from '@/lib/icons'
import { cn } from '@/lib/utils'
import { notify, notifyError } from '@/store/notifications'
import { $startupShell, closeStartupGuide, completeStartupGuide } from '@/store/startup'

import { DesktopController } from './desktop-controller'
import { hasColdCache } from '@/lib/cache-first-bootstrap'
import { scheduleBackgroundPrewarm } from '@/lib/bg-prewarm'

type RemoteAuthMode = 'oauth' | 'token'
type StartupMode = 'local' | 'remote'
type StartupPhase =
  | 'mode_gate'
  | 'local_checking'
  | 'local_failed'
  | 'local_install_required'
  | 'local_installing'
  | 'local_ready'
  | 'remote_editing'
  | 'remote_failed'
  | 'remote_ready'
  | 'remote_testing'

type ProbeStatus = 'done' | 'error' | 'idle' | 'probing'

interface RemoteConfigState {
  envOverride: boolean
  mode: 'local' | 'remote'
  remoteAuthMode: RemoteAuthMode
  remoteOauthConnected: boolean
  remoteTokenPreview: null | string
  remoteTokenSet: boolean
  remoteUrl: string
}

interface ChoiceCardProps {
  description: string
  icon: typeof Monitor
  onClick: () => void
  title: string
}

interface SourceOptionProps<T extends string> {
  active: boolean
  disabled?: boolean
  label: string
  onClick: () => void
  value: T
}

const DEFAULT_UPDATE_SOURCES: DesktopUpdateSourceConfig = {
  agentGitCustomUrl: '',
  agentGitSource: 'gitee',
  desktopRepoUrl: 'https://github.com/meespace/hermes-desktop-tauri',
  npmCustomUrl: '',
  npmSource: 'npmjs',
  pythonCustomUrl: '',
  pythonSource: 'pypi'
}

function normalizeUpdateSources(config: DesktopUpdateSourceConfig): DesktopUpdateSourceConfig {
  return config.pythonSource === 'tsinghua' ? { ...config, pythonSource: 'pypi' } : config
}

const DEFAULT_REMOTE_CONFIG: RemoteConfigState = {
  envOverride: false,
  mode: 'remote',
  remoteAuthMode: 'token',
  remoteOauthConnected: false,
  remoteTokenPreview: null,
  remoteTokenSet: false,
  remoteUrl: ''
}

const FALLBACK_INSTALL_COMMAND =
  'curl -fsSL https://raw.githubusercontent.com/NousResearch/hermes-agent/main/scripts/install.sh | bash -s -- --include-desktop'
const FALLBACK_INSTALL_DOCS_URL = 'https://hermes-agent.nousresearch.com/docs/'
const DEFAULT_AGENT_GIT_URL = 'https://github.com/NousResearch/hermes-agent.git'
const GITEE_AGENT_GIT_URL = 'https://gitee.com/8187735/hermes-agent.git'
const GITCODE_AGENT_GIT_URL = 'https://gitcode.com/macaque_zhang/hermes-agent.git'
const DEFAULT_PYTHON_INDEX_URL = 'https://pypi.org/simple'
const ALIYUN_PYTHON_INDEX_URL = 'https://mirrors.aliyun.com/pypi/simple/'
const DEFAULT_NPM_REGISTRY_URL = 'https://registry.npmjs.org/'
const NPMMIRROR_REGISTRY_URL = 'https://registry.npmmirror.com/'

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}

function isInstallRequiredError(message: string) {
  const normalized = message.toLowerCase()
  return (
    normalized.includes('not installed yet') ||
    normalized.includes('run `') ||
    normalized.includes('hermes cli') ||
    normalized.includes('missing desktop routes') ||
    normalized.includes('upgrade hermes')
  )
}

function buildInstallCommand(command: string, sources: DesktopUpdateSourceConfig) {
  const envParts: string[] = []

  if (sources.agentGitSource === 'custom' && sources.agentGitCustomUrl.trim()) {
    envParts.push(`HERMES_AGENT_GIT_URL='${sources.agentGitCustomUrl.trim()}'`)
  } else if (sources.agentGitSource === 'gitcode') {
    envParts.push(`HERMES_AGENT_GIT_URL='${GITCODE_AGENT_GIT_URL}'`)
  } else if (sources.agentGitSource === 'gitee') {
    envParts.push(`HERMES_AGENT_GIT_URL='${GITEE_AGENT_GIT_URL}'`)
  } else {
    envParts.push(`HERMES_AGENT_GIT_URL='${DEFAULT_AGENT_GIT_URL}'`)
  }

  const pythonIndexUrl =
    sources.pythonSource === 'aliyun'
        ? ALIYUN_PYTHON_INDEX_URL
        : sources.pythonSource === 'custom'
          ? sources.pythonCustomUrl.trim()
          : DEFAULT_PYTHON_INDEX_URL

  if (pythonIndexUrl) {
    envParts.push(`PIP_INDEX_URL='${pythonIndexUrl}'`)
    envParts.push(`UV_DEFAULT_INDEX='${pythonIndexUrl}'`)
  }

  const npmRegistryUrl =
    sources.npmSource === 'npmmirror'
      ? NPMMIRROR_REGISTRY_URL
      : sources.npmSource === 'custom'
        ? sources.npmCustomUrl.trim()
        : DEFAULT_NPM_REGISTRY_URL

  if (npmRegistryUrl) {
    envParts.push(`npm_config_registry='${npmRegistryUrl}'`)
    envParts.push(`NPM_CONFIG_REGISTRY='${npmRegistryUrl}'`)
  }

  return `${envParts.join(' ')} ${command}`.trim()
}

function ChoiceCard({ description, icon: Icon, onClick, title }: ChoiceCardProps) {
  return (
    <button
      className="group relative overflow-hidden rounded-[1.4rem] border border-[color-mix(in_srgb,var(--accent)_10%,transparent)] bg-[color-mix(in_srgb,var(--workbench-panel-bg)_94%,white_6%)] p-5 text-left shadow-[0_20px_60px_rgba(15,23,42,0.08)] transition duration-200 hover:-translate-y-0.5 hover:border-[color-mix(in_srgb,var(--accent)_18%,transparent)] hover:bg-[color-mix(in_srgb,var(--workbench-hover)_82%,white_10%)]"
      onClick={onClick}
      type="button"
    >
      <div className="absolute inset-x-0 top-0 h-24 bg-[radial-gradient(circle_at_top_left,color-mix(in_srgb,var(--accent)_14%,transparent),transparent_58%)] opacity-80" />
      <div className="relative">
        <div className="flex items-center gap-3">
          <span className="grid size-11 place-items-center rounded-[1rem] border border-[color-mix(in_srgb,var(--accent)_14%,transparent)] bg-[color-mix(in_srgb,var(--surface)_78%,white_22%)] text-[var(--accent)]">
            <Icon className="size-5" />
          </span>
          <div>
            <h3 className="text-[1rem] font-semibold tracking-[-0.03em] text-[var(--foreground)]">{title}</h3>
            <p className="mt-1 text-[0.72rem] leading-5 text-[color-mix(in_srgb,var(--foreground)_64%,transparent)]">
              {description}
            </p>
          </div>
        </div>

        <div className="mt-5 flex items-center gap-2 text-[0.72rem] font-medium text-[var(--accent)]">
          <span>继续</span>
          <ArrowUpRight className="size-3.5 transition group-hover:translate-x-0.5" />
        </div>
      </div>
    </button>
  )
}

function SourceOptionButton<T extends string>({
  active,
  disabled,
  label,
  onClick
}: Omit<SourceOptionProps<T>, 'value'>) {
  return (
    <button
      className={cn(
        'rounded-[0.8rem] border px-3 py-2 text-left text-[0.72rem] font-medium transition',
        active
          ? 'border-[color-mix(in_srgb,var(--accent)_18%,transparent)] bg-[var(--workbench-active)] text-[var(--foreground)]'
          : disabled
            ? 'cursor-not-allowed border-[color-mix(in_srgb,var(--workbench-divider)_94%,transparent)] bg-[color-mix(in_srgb,var(--surface)_92%,transparent)] text-[color-mix(in_srgb,var(--foreground)_38%,transparent)] opacity-70'
            : 'border-[color-mix(in_srgb,var(--workbench-divider)_94%,transparent)] bg-[color-mix(in_srgb,var(--surface)_92%,transparent)] text-[var(--muted)] hover:bg-[var(--workbench-hover)]'
      )}
      disabled={disabled}
      onClick={onClick}
      type="button"
    >
      {label}
    </button>
  )
}

function StartupSurface({
  children,
  eyebrow,
  headerActions,
  title,
  description
}: {
  children: ReactNode
  description: string
  eyebrow: string
  headerActions?: ReactNode
  title: string
}) {
  return (
    <div className="relative min-h-screen overflow-x-hidden overflow-y-auto bg-[linear-gradient(180deg,color-mix(in_srgb,var(--workbench-shell-bg)_92%,white_8%),var(--workbench-shell-bg))]">
      <div className="pointer-events-none absolute inset-0 bg-[radial-gradient(circle_at_top_left,color-mix(in_srgb,var(--accent)_12%,transparent),transparent_30%),radial-gradient(circle_at_bottom_right,color-mix(in_srgb,var(--accent)_8%,transparent),transparent_26%)]" />
      <div className="relative mx-auto flex min-h-screen w-full max-w-6xl items-start px-4 py-6 sm:px-6 sm:py-8 lg:px-8 lg:py-10">
        <div
          className="flex max-h-[calc(100vh-3rem)] w-full flex-col overflow-hidden rounded-[2rem] border border-[color-mix(in_srgb,var(--workbench-panel-stroke)_92%,transparent)] bg-[color-mix(in_srgb,var(--workbench-panel-bg)_94%,white_6%)] shadow-[0_24px_80px_rgba(15,23,42,0.12)] backdrop-blur-xl sm:max-h-[calc(100vh-4rem)]"
          data-slot="startup-surface-card"
        >
          <div className="border-b border-[var(--workbench-divider)] px-6 py-5 sm:px-8">
            <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
              <div className="min-w-0">
                <div className="inline-flex items-center gap-2 rounded-full border border-[color-mix(in_srgb,var(--accent)_14%,transparent)] bg-[color-mix(in_srgb,var(--accent)_6%,transparent)] px-2.5 py-1 text-[0.62rem] font-semibold uppercase tracking-[0.16em] text-[var(--accent)]">
                  <span aria-hidden="true" className="size-1.5 rounded-full bg-[var(--accent)]" />
                  {eyebrow}
                </div>
                <h1 className="mt-4 text-[1.6rem] font-semibold tracking-[-0.05em] text-[var(--foreground)] sm:text-[1.9rem]">
                  {title}
                </h1>
                <p className="mt-2 max-w-2xl text-[0.78rem] leading-6 text-[color-mix(in_srgb,var(--foreground)_62%,transparent)]">
                  {description}
                </p>
              </div>
              {headerActions ? <div className="shrink-0">{headerActions}</div> : null}
            </div>
          </div>

          <div
            className="min-h-0 flex-1 overflow-y-auto px-6 py-6 sm:px-8 sm:py-7"
            data-slot="startup-surface-viewport"
          >
            {children}
          </div>
        </div>
      </div>
    </div>
  )
}

export function StartupController() {
  const { t } = useI18n()
  const startupShell = useStore($startupShell)
  const desktop = window.hermesDesktop
  const probeSeq = useRef(0)
  const cacheFastPath = useMemo(() => hasColdCache(), [])
  const updateSourceCopy = t.settings.updateSources

  useEffect(() => {
    if (!cacheFastPath) return
    return scheduleBackgroundPrewarm({})
  }, [cacheFastPath])

  const [phase, setPhase] = useState<StartupPhase>('mode_gate')
  const [selectedMode, setSelectedMode] = useState<null | StartupMode>(null)
  const [statusMessage, setStatusMessage] = useState<string | null>(null)
  const [errorDetail, setErrorDetail] = useState<string | null>(null)
  const [sourcesOpen, setSourcesOpen] = useState(false)
  const [sourcesLoading, setSourcesLoading] = useState(true)
  const [sourcesSaving, setSourcesSaving] = useState(false)
  const [sources, setSources] = useState<DesktopUpdateSourceConfig>(DEFAULT_UPDATE_SOURCES)
  const [remoteLoading, setRemoteLoading] = useState(true)
  const [remoteState, setRemoteState] = useState<RemoteConfigState>(DEFAULT_REMOTE_CONFIG)
  const [remoteToken, setRemoteToken] = useState('')
  const [probeStatusState, setProbeStatusState] = useState<ProbeStatus>('idle')
  const [probeState, setProbeState] = useState<DesktopConnectionProbeResult | null>(null)
  const [signingIn, setSigningIn] = useState(false)
  const [installCommand, setInstallCommand] = useState(FALLBACK_INSTALL_COMMAND)
  const [installDocsUrl, setInstallDocsUrl] = useState(FALLBACK_INSTALL_DOCS_URL)

  useEffect(() => {
    if (!startupShell.visible) {
      return
    }

    setPhase('mode_gate')
    setSelectedMode(null)
    setStatusMessage(null)
    setErrorDetail(null)
    setSourcesOpen(false)
    setSourcesLoading(true)
    setRemoteLoading(true)
    setRemoteState(DEFAULT_REMOTE_CONFIG)
    setRemoteToken('')
    setProbeStatusState('idle')
    setProbeState(null)
    setSigningIn(false)
    setInstallCommand(FALLBACK_INSTALL_COMMAND)
    setInstallDocsUrl(FALLBACK_INSTALL_DOCS_URL)

    if (!desktop) {
      setSourcesLoading(false)
      setRemoteLoading(false)
      return
    }

    let cancelled = false

    void desktop.updates
      .getSources()
      .then(config => {
        if (!cancelled) {
          setSources(normalizeUpdateSources({ ...DEFAULT_UPDATE_SOURCES, ...config }))
        }
      })
      .catch(error => notifyError(error, '读取安装源配置失败'))
      .finally(() => {
        if (!cancelled) {
          setSourcesLoading(false)
        }
      })

    void desktop
      .getConnectionConfig()
      .then(config => {
        if (!cancelled) {
          setRemoteState({
            envOverride: config.envOverride,
            mode: config.mode,
            remoteAuthMode: config.remoteAuthMode,
            remoteOauthConnected: config.remoteOauthConnected,
            remoteTokenPreview: config.remoteTokenPreview,
            remoteTokenSet: config.remoteTokenSet,
            remoteUrl: config.remoteUrl
          })
        }
      })
      .catch(error => notifyError(error, '读取远程连接配置失败'))
      .finally(() => {
        if (!cancelled) {
          setRemoteLoading(false)
        }
      })

    const syncBootstrap = (snapshot: DesktopBootstrapState | null) => {
      const unsupported = snapshot?.unsupportedPlatform

      if (!unsupported) {
        return
      }

      setInstallCommand(unsupported.installCommand || FALLBACK_INSTALL_COMMAND)
      setInstallDocsUrl(unsupported.docsUrl || FALLBACK_INSTALL_DOCS_URL)
    }

    void desktop
      .getBootstrapState?.()
      .then(snapshot => {
        if (!cancelled) {
          syncBootstrap(snapshot)
        }
      })
      .catch(() => undefined)

    const offBootstrap = desktop.onBootstrapEvent?.(() => {
      void desktop
        .getBootstrapState?.()
        .then(snapshot => {
          if (!cancelled) {
            syncBootstrap(snapshot)
          }
        })
        .catch(() => undefined)
    })

    return () => {
      cancelled = true
      offBootstrap?.()
    }
  }, [desktop, startupShell.resetToken, startupShell.visible])

  const trimmedRemoteUrl = remoteState.remoteUrl.trim()
  const shouldProbe = phase.startsWith('remote') && /^https?:\/\//i.test(trimmedRemoteUrl) && Boolean(desktop?.probeConnectionConfig)

  useEffect(() => {
    if (!shouldProbe || !desktop) {
      setProbeStatusState('idle')
      setProbeState(null)
      return
    }

    const seq = ++probeSeq.current
    setProbeStatusState('probing')

    const timer = window.setTimeout(() => {
      desktop
        .probeConnectionConfig(trimmedRemoteUrl)
        .then(result => {
          if (seq !== probeSeq.current) {
            return
          }

          setProbeState(result)
          setProbeStatusState(result.reachable ? 'done' : 'error')
        })
        .catch(() => {
          if (seq !== probeSeq.current) {
            return
          }

          setProbeState(null)
          setProbeStatusState('error')
        })
    }, 450)

    return () => window.clearTimeout(timer)
  }, [desktop, shouldProbe, trimmedRemoteUrl])

  const authMode: RemoteAuthMode = useMemo(() => {
    if (probeStatusState === 'done' && probeState && probeState.authMode !== 'unknown') {
      return probeState.authMode
    }

    return remoteState.remoteAuthMode
  }, [probeState, probeStatusState, remoteState.remoteAuthMode])

  const canUseRemote = useMemo(() => {
    if (!trimmedRemoteUrl) {
      return false
    }

    if (authMode === 'oauth') {
      return remoteState.remoteOauthConnected
    }

    return Boolean(remoteToken.trim()) || remoteState.remoteTokenSet
  }, [authMode, remoteState.remoteOauthConnected, remoteState.remoteTokenSet, remoteToken, trimmedRemoteUrl])

  const effectiveInstallCommand = useMemo(() => buildInstallCommand(installCommand, sources), [installCommand, sources])

  useEffect(() => {
    if (phase !== 'local_ready' && phase !== 'remote_ready') {
      return
    }

    const timer = window.setTimeout(() => completeStartupGuide(), 1000)
    return () => window.clearTimeout(timer)
  }, [phase])

  const goBackToModeGate = () => {
    setPhase('mode_gate')
    setSelectedMode(null)
    setStatusMessage(null)
    setErrorDetail(null)
  }

  const saveSources = async () => {
    if (!desktop) {
      return
    }

    return desktop.updates.setSources(sources)
  }

  const persistSources = async (showToast = true) => {
    if (!desktop) {
      return
    }

    setSourcesSaving(true)

    try {
      const next = await saveSources()
      if (!next) {
        return
      }
      setSources(normalizeUpdateSources({ ...DEFAULT_UPDATE_SOURCES, ...next }))
      if (showToast) {
        notify({ kind: 'success', title: updateSourceCopy.startupSaveTitle, message: updateSourceCopy.startupSaveMessage })
      }
    } catch (error) {
      notifyError(error, updateSourceCopy.startupSaveFailed)
      throw error
    } finally {
      setSourcesSaving(false)
    }
  }

  const runLocalCheck = async () => {
    if (!desktop?.applyConnectionConfig || !desktop.testConnectionConfig) {
      setPhase('local_failed')
      setErrorDetail('当前桌面端未暴露本地检测接口。')
      return
    }

    setSelectedMode('local')
    setPhase('local_checking')
    setStatusMessage(null)
    setErrorDetail(null)

    try {
      await desktop.applyConnectionConfig({ mode: 'local' })
      const result = await desktop.testConnectionConfig({ mode: 'local' })
      setStatusMessage(result.version ? `检测完成，Hermes ${result.version} 已可用。` : `检测完成，${result.baseUrl} 已可用。`)
      setPhase('local_ready')
    } catch (error) {
      const message = errorMessage(error)
      setErrorDetail(message)
      setPhase(isInstallRequiredError(message) ? 'local_install_required' : 'local_failed')
    }
  }

  const installLocalHermes = async () => {
    await persistSources(false).catch(() => undefined)
    await copyInstallCommand()
  }

  const startRemoteFlow = () => {
    setSelectedMode('remote')
    setPhase('remote_editing')
    setStatusMessage(null)
    setErrorDetail(null)
  }

  const signInRemote = async () => {
    if (!desktop?.oauthLoginConnectionConfig || !desktop.saveConnectionConfig || !desktop.getConnectionConfig) {
      notify({ kind: 'warning', title: '当前版本不支持 OAuth 登录', message: '请改用 Token，或升级桌面端后再试。' })
      return
    }

    if (!trimmedRemoteUrl) {
      notify({ kind: 'warning', title: '请先填写远程地址', message: '需要先提供可访问的 Hermes 网关地址。' })
      return
    }

    setSigningIn(true)

    try {
      await desktop.saveConnectionConfig({
        mode: 'remote',
        remoteAuthMode: 'oauth',
        remoteUrl: trimmedRemoteUrl
      })

      const result = await desktop.oauthLoginConnectionConfig(trimmedRemoteUrl)

      if (result.connected) {
        const refreshed = await desktop.getConnectionConfig()
        setRemoteState({
          envOverride: refreshed.envOverride,
          mode: refreshed.mode,
          remoteAuthMode: refreshed.remoteAuthMode,
          remoteOauthConnected: refreshed.remoteOauthConnected,
          remoteTokenPreview: refreshed.remoteTokenPreview,
          remoteTokenSet: refreshed.remoteTokenSet,
          remoteUrl: refreshed.remoteUrl
        })
        notify({ kind: 'success', title: '远程账号已连接', message: `已连接到 ${result.baseUrl}` })
      } else {
        notify({ kind: 'warning', title: '登录尚未完成', message: '完成浏览器授权后，再回来测试连接。' })
      }
    } catch (error) {
      notifyError(error, '远程登录失败')
    } finally {
      setSigningIn(false)
    }
  }

  const signOutRemote = async () => {
    if (!desktop?.oauthLogoutConnectionConfig || !desktop.getConnectionConfig) {
      return
    }

    setSigningIn(true)

    try {
      await desktop.oauthLogoutConnectionConfig(trimmedRemoteUrl || undefined)
      const refreshed = await desktop.getConnectionConfig()
      setRemoteState({
        envOverride: refreshed.envOverride,
        mode: refreshed.mode,
        remoteAuthMode: refreshed.remoteAuthMode,
        remoteOauthConnected: refreshed.remoteOauthConnected,
        remoteTokenPreview: refreshed.remoteTokenPreview,
        remoteTokenSet: refreshed.remoteTokenSet,
        remoteUrl: refreshed.remoteUrl
      })
      notify({ kind: 'success', title: '已断开远程账号', message: '你可以重新登录，或切换为 Token 模式。' })
    } catch (error) {
      notifyError(error, '退出远程登录失败')
    } finally {
      setSigningIn(false)
    }
  }

  const testRemote = async () => {
    if (!desktop?.saveConnectionConfig || !desktop.applyConnectionConfig || !desktop.testConnectionConfig) {
      setPhase('remote_failed')
      setErrorDetail('当前桌面端未暴露远程连接接口。')
      return
    }

    if (!canUseRemote) {
      notify({
        kind: 'warning',
        title: '远程信息还不完整',
        message: authMode === 'oauth' ? '请先完成登录，再测试连接。' : '请填写访问令牌后，再测试连接。'
      })
      return
    }

    const payload = {
      mode: 'remote' as const,
      remoteAuthMode: authMode,
      remoteToken: authMode === 'token' ? remoteToken.trim() || undefined : undefined,
      remoteUrl: trimmedRemoteUrl
    }

    setPhase('remote_testing')
    setStatusMessage(null)
    setErrorDetail(null)

    try {
      await desktop.saveConnectionConfig(payload)
      const result = await desktop.testConnectionConfig(payload)
      await desktop.applyConnectionConfig(payload)
      setStatusMessage(result.version ? `连接成功，Hermes ${result.version} 已可用。` : `连接成功，${result.baseUrl} 已可用。`)
      setPhase('remote_ready')
    } catch (error) {
      setErrorDetail(errorMessage(error))
      setPhase('remote_failed')
    }
  }

  const openLogs = async () => {
    try {
      await desktop?.revealLogs()
    } catch (error) {
      notifyError(error, '打开日志目录失败')
    }
  }

  const copyInstallCommand = async () => {
    try {
      await desktop?.writeClipboard(effectiveInstallCommand)
      notify({ kind: 'success', title: '安装命令已复制', message: '在终端执行后，回到这里点“重新检测”即可。' })
    } catch (error) {
      notifyError(error, '复制安装命令失败')
    }
  }

  const headerActions = (
    <div className="flex flex-wrap items-center gap-2">
      {startupShell.canReturnToApp ? (
        <Button onClick={() => closeStartupGuide()} size="sm" variant="outline">
          <ChevronLeft className="size-4" />
          返回应用
        </Button>
      ) : null}
      {selectedMode ? (
        <Button onClick={goBackToModeGate} size="sm" variant="ghost">
          <ChevronLeft className="size-4" />
          返回模式选择
        </Button>
      ) : null}
    </div>
  )

  if (cacheFastPath) {
    return <DesktopController />
  }

  if (!startupShell.visible && startupShell.entered) {
    return <DesktopController />
  }

  const renderLocalSources = (
    <div className="rounded-[1rem] border border-[color-mix(in_srgb,var(--workbench-divider)_94%,transparent)] bg-[color-mix(in_srgb,var(--surface)_92%,transparent)] p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <div className="text-[0.82rem] font-semibold text-[var(--foreground)]">{updateSourceCopy.startupPanelTitle}</div>
          <p className="mt-1 text-[0.72rem] leading-5 text-[var(--muted)]">
            {updateSourceCopy.startupPanelDescription}
          </p>
        </div>
        <Button
          disabled={sourcesLoading || sourcesSaving}
          onClick={() => void persistSources()}
          size="sm"
          variant="outline"
        >
          {sourcesSaving ? <Loader2 className="size-4 animate-spin" /> : <Settings2 className="size-4" />}
          {sourcesSaving ? updateSourceCopy.saving : updateSourceCopy.startupSave}
        </Button>
      </div>

      {sourcesLoading ? (
        <div className="mt-4 flex items-center gap-2 text-[0.74rem] text-[var(--muted)]">
          <Loader2 className="size-4 animate-spin" />
          {updateSourceCopy.startupLoading}
        </div>
      ) : (
        <div className="mt-4 grid gap-4">
          <div className="grid gap-2">
            <div className="text-[0.72rem] font-medium text-[var(--foreground)]">{updateSourceCopy.agentTitle}</div>
            <div className="flex flex-wrap gap-2">
              <SourceOptionButton
                active={sources.agentGitSource === 'github'}
                label={updateSourceCopy.githubGit}
                onClick={() => setSources(current => ({ ...current, agentGitSource: 'github' }))}
              />
              <SourceOptionButton
                active={sources.agentGitSource === 'gitee'}
                label={updateSourceCopy.giteeGit}
                onClick={() => setSources(current => ({ ...current, agentGitSource: 'gitee' }))}
              />
              <SourceOptionButton
                active={sources.agentGitSource === 'gitcode'}
                label={updateSourceCopy.gitcodeGit}
                onClick={() => setSources(current => ({ ...current, agentGitSource: 'gitcode' }))}
              />
              <SourceOptionButton
                active={sources.agentGitSource === 'custom'}
                label={updateSourceCopy.custom}
                onClick={() => setSources(current => ({ ...current, agentGitSource: 'custom' }))}
              />
            </div>
            {sources.agentGitSource === 'custom' ? (
              <Input
                className="h-10 text-[0.74rem]"
                onChange={event => setSources(current => ({ ...current, agentGitCustomUrl: event.target.value }))}
                placeholder="https://gitee.com/your-org/hermes-agent.git"
                value={sources.agentGitCustomUrl}
              />
            ) : null}
          </div>

          <div className="grid gap-2">
            <div className="text-[0.72rem] font-medium text-[var(--foreground)]">{updateSourceCopy.pythonTitle}</div>
            <div className="flex flex-wrap gap-2">
              <SourceOptionButton
                active={sources.pythonSource === 'pypi'}
                label={updateSourceCopy.pypi}
                onClick={() => setSources(current => ({ ...current, pythonSource: 'pypi' }))}
              />
              <SourceOptionButton
                active={sources.pythonSource === 'tsinghua'}
                label={updateSourceCopy.tsinghua}
                disabled
                onClick={() => undefined}
              />
              <SourceOptionButton
                active={sources.pythonSource === 'aliyun'}
                label={updateSourceCopy.aliyun}
                onClick={() => setSources(current => ({ ...current, pythonSource: 'aliyun' }))}
              />
              <SourceOptionButton
                active={sources.pythonSource === 'custom'}
                label={updateSourceCopy.custom}
                onClick={() => setSources(current => ({ ...current, pythonSource: 'custom' }))}
              />
            </div>
            {sources.pythonSource === 'custom' ? (
              <Input
                className="h-10 text-[0.74rem]"
                onChange={event => setSources(current => ({ ...current, pythonCustomUrl: event.target.value }))}
                placeholder="https://example.com/pypi/simple"
                value={sources.pythonCustomUrl}
              />
            ) : null}
          </div>

          <div className="grid gap-2">
            <div className="text-[0.72rem] font-medium text-[var(--foreground)]">{updateSourceCopy.npmTitle}</div>
            <div className="flex flex-wrap gap-2">
              <SourceOptionButton
                active={sources.npmSource === 'npmjs'}
                label={updateSourceCopy.npmjs}
                onClick={() => setSources(current => ({ ...current, npmSource: 'npmjs' }))}
              />
              <SourceOptionButton
                active={sources.npmSource === 'npmmirror'}
                label={updateSourceCopy.npmmirror}
                onClick={() => setSources(current => ({ ...current, npmSource: 'npmmirror' }))}
              />
              <SourceOptionButton
                active={sources.npmSource === 'custom'}
                label={updateSourceCopy.custom}
                onClick={() => setSources(current => ({ ...current, npmSource: 'custom' }))}
              />
            </div>
            {sources.npmSource === 'custom' ? (
              <Input
                className="h-10 text-[0.74rem]"
                onChange={event => setSources(current => ({ ...current, npmCustomUrl: event.target.value }))}
                placeholder="https://registry.example.com/"
                value={sources.npmCustomUrl}
              />
            ) : null}
          </div>
        </div>
      )}
    </div>
  )

  const renderModeGate = (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)] lg:items-start">
      <div className="rounded-[1.4rem] border border-[color-mix(in_srgb,var(--workbench-divider)_92%,transparent)] bg-[color-mix(in_srgb,var(--surface)_90%,transparent)] p-5">
        <div className="flex items-center gap-2 text-[0.78rem] font-medium text-[var(--foreground)]">
          <CheckCircle2 className="size-4 text-[var(--accent)]" />
          启动前先确认这次如何连接 Hermes
        </div>
        <p className="mt-3 text-[0.74rem] leading-6 text-[var(--muted)]">
          本地模式会优先检测本机 Hermes Agent；远程模式只负责配置并验证远程网关。模型提供商配置改为进入应用后按需处理。
        </p>
        <div className="mt-5 grid gap-3">
          <div className="rounded-[1rem] border border-[color-mix(in_srgb,var(--accent)_12%,transparent)] bg-[color-mix(in_srgb,var(--accent)_5%,transparent)] p-4">
            <div className="text-[0.74rem] font-medium text-[var(--foreground)]">本次重构后的启动原则</div>
            <div className="mt-2 text-[0.7rem] leading-5 text-[var(--muted)]">
              首次进入或手动重新切换模式时再打开这里，选完之后只做当前模式该做的检查，不再把远程、本地和 provider 配置混在一起。
            </div>
          </div>
        </div>
      </div>

      <div className="grid gap-4">
        <ChoiceCard
          description="优先使用本机 Hermes Agent。若未安装或状态异常，会在下一步给出修复与安装指引。"
          icon={Monitor}
          onClick={() => void runLocalCheck()}
          title="本地模式"
        />
        <ChoiceCard
          description="使用远程 Hermes 网关。下一步继续填写地址、认证信息并测试连接。"
          icon={Globe}
          onClick={startRemoteFlow}
          title="远程模式"
        />
      </div>
    </div>
  )

  const renderLocalState = (
    <div className="grid gap-5 lg:grid-cols-[minmax(0,0.82fr)_minmax(0,1.18fr)]">
      <div className="rounded-[1.4rem] border border-[color-mix(in_srgb,var(--workbench-divider)_92%,transparent)] bg-[color-mix(in_srgb,var(--surface)_90%,transparent)] p-5">
        <div className="flex items-center gap-3">
          <span className="grid size-11 place-items-center rounded-[1rem] border border-[color-mix(in_srgb,var(--accent)_14%,transparent)] bg-[color-mix(in_srgb,var(--surface)_78%,white_22%)] text-[var(--accent)]">
            <Monitor className="size-5" />
          </span>
          <div>
            <div className="text-[0.96rem] font-semibold tracking-[-0.02em] text-[var(--foreground)]">本地模式</div>
            <div className="mt-1 text-[0.72rem] text-[var(--muted)]">优先检查并使用本机 Hermes Agent。</div>
          </div>
        </div>

        <div className="mt-5 rounded-[1rem] border border-[color-mix(in_srgb,var(--accent)_12%,transparent)] bg-[color-mix(in_srgb,var(--accent)_5%,transparent)] p-4">
          <div className="text-[0.74rem] font-medium text-[var(--foreground)]">当前阶段</div>
          <div className="mt-2 text-[0.72rem] leading-5 text-[var(--muted)]">
            {phase === 'local_checking'
              ? '正在检查 Hermes Agent 是否已安装，以及本地网关是否能正常拉起。'
              : phase === 'local_installing'
                ? '正在整理系统 Hermes 的安装或升级指引。'
              : phase === 'local_install_required'
                ? '当前机器还没有可用的 Hermes，或者版本缺少桌面端需要的接口。先手动安装或升级系统 Hermes，再回来重新检测。'
                : phase === 'local_ready'
                  ? '本地环境检测通过，可以直接进入应用。'
                  : '检测到了本地环境异常，你可以重新检测、查看日志，或者回到模式选择。'}
          </div>
        </div>
      </div>

      <div className="rounded-[1.4rem] border border-[color-mix(in_srgb,var(--workbench-divider)_92%,transparent)] bg-[var(--workbench-panel-bg)] p-5">
        {phase === 'local_checking' ? (
          <div className="flex min-h-[18rem] flex-col items-center justify-center text-center">
            <div className="grid size-16 place-items-center rounded-full border border-[color-mix(in_srgb,var(--accent)_16%,transparent)] bg-[color-mix(in_srgb,var(--accent)_8%,transparent)] text-[var(--accent)]">
              <Loader2 className="size-7 animate-spin" />
            </div>
            <div className="mt-5 text-[1rem] font-semibold text-[var(--foreground)]">正在检测本地 Hermes Agent</div>
            <div className="mt-2 max-w-md text-[0.76rem] leading-6 text-[var(--muted)]">
              会依次确认本地 CLI、网关启动和状态接口是否正常。
            </div>
          </div>
        ) : null}

        {phase === 'local_ready' ? (
          <div className="grid gap-5">
            <div className="flex items-start gap-3 rounded-[1rem] border border-emerald-500/20 bg-emerald-500/10 p-4 text-emerald-700">
              <Check className="mt-0.5 size-5 shrink-0" />
              <div>
                <div className="text-[0.86rem] font-semibold">检测完成</div>
                <div className="mt-1 text-[0.74rem] leading-5">{statusMessage}</div>
              </div>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button onClick={() => completeStartupGuide()}>
                <Sparkles className="size-4" />
                立即进入
              </Button>
              <Button onClick={() => void runLocalCheck()} variant="outline">
                <RefreshCw className="size-4" />
                重新检测
              </Button>
            </div>
            <p className="text-[0.7rem] leading-5 text-[var(--muted)]">若无操作，将在 1 秒后自动进入应用。</p>
          </div>
        ) : null}

        {phase === 'local_installing' ? (
          <div className="flex min-h-[18rem] flex-col items-center justify-center text-center">
            <div className="grid size-16 place-items-center rounded-full border border-[color-mix(in_srgb,var(--accent)_16%,transparent)] bg-[color-mix(in_srgb,var(--accent)_8%,transparent)] text-[var(--accent)]">
              <Loader2 className="size-7 animate-spin" />
            </div>
            <div className="mt-5 text-[1rem] font-semibold text-[var(--foreground)]">正在准备系统 Hermes 指引</div>
            <div className="mt-2 max-w-md text-[0.76rem] leading-6 text-[var(--muted)]">
              桌面端不会再额外安装第二套 Hermes，只会生成系统 Hermes 的安装或升级命令。
            </div>
          </div>
        ) : null}

        {phase === 'local_failed' ? (
          <div className="grid gap-4">
            <div className="flex items-start gap-3 rounded-[1rem] border border-[color-mix(in_srgb,var(--danger)_18%,transparent)] bg-[var(--danger-soft)] p-4 text-[var(--danger)]">
              <AlertCircle className="mt-0.5 size-5 shrink-0" />
              <div>
                <div className="text-[0.86rem] font-semibold">本机 Hermes 没有正常启动</div>
                <div className="mt-1 text-[0.74rem] leading-5">{errorDetail || '本地健康检查未通过。'}</div>
              </div>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button onClick={() => void runLocalCheck()}>
                <RefreshCw className="size-4" />
                重新检测
              </Button>
              <Button
                onClick={() => {
                  setPhase('local_install_required')
                  setSourcesOpen(true)
                }}
                variant="outline"
              >
                查看安装指引
              </Button>
              <Button onClick={() => void openLogs()} variant="ghost">
                查看日志
              </Button>
            </div>
          </div>
        ) : null}

        {phase === 'local_install_required' ? (
          <div className="grid gap-4">
            <div className="flex items-start gap-3 rounded-[1rem] border border-[color-mix(in_srgb,var(--warning)_18%,transparent)] bg-[color-mix(in_srgb,var(--warning)_10%,transparent)] p-4 text-[var(--foreground)]">
              <AlertCircle className="mt-0.5 size-5 shrink-0 text-[var(--accent)]" />
              <div>
                <div className="text-[0.86rem] font-semibold">还没有检测到本机 Hermes Agent</div>
                <div className="mt-1 text-[0.74rem] leading-5 text-[var(--muted)]">
                  桌面端不会额外安装第二套 Hermes。请先手动安装或升级你系统里的 Hermes，然后回来点“重新检测”。
                </div>
              </div>
            </div>

            <div className="rounded-[1rem] border border-[color-mix(in_srgb,var(--workbench-divider)_92%,transparent)] bg-[color-mix(in_srgb,var(--surface)_92%,transparent)] p-4">
              <div className="text-[0.76rem] font-medium text-[var(--foreground)]">安装命令</div>
              <pre className="mt-3 overflow-x-auto rounded-[0.9rem] border border-[var(--workbench-divider)] bg-[var(--surface-secondary)] p-3 text-[0.7rem] leading-5 text-[var(--muted)]">
                {effectiveInstallCommand}
              </pre>
            </div>

            {errorDetail ? (
              <div className="rounded-[1rem] border border-[color-mix(in_srgb,var(--danger)_18%,transparent)] bg-[var(--danger-soft)] p-4 text-[0.74rem] leading-5 text-[var(--danger)]">
                {errorDetail}
              </div>
            ) : null}

            <div className="flex flex-wrap gap-2">
              <Button onClick={() => void installLocalHermes()}>
                复制安装命令
              </Button>
              <Button onClick={() => void runLocalCheck()} variant="outline">
                <RefreshCw className="size-4" />
                重新检测
              </Button>
              <Button
                onClick={() => {
                  if (installDocsUrl) {
                    void desktop?.openExternal(installDocsUrl)
                  }
                }}
                variant="ghost"
              >
                查看安装文档
              </Button>
              <Button onClick={() => void openLogs()} variant="ghost">
                查看日志
              </Button>
            </div>

            <div className="grid gap-3">
              <button
                className="flex items-center gap-2 self-start text-[0.72rem] font-medium text-[var(--accent)] transition hover:opacity-80"
                onClick={() => setSourcesOpen(current => !current)}
                type="button"
              >
                <Settings2 className="size-4" />
                {sourcesOpen ? '收起安装源设置' : '展开安装源设置'}
              </button>
              {sourcesOpen ? renderLocalSources : null}
            </div>
          </div>
        ) : null}
      </div>
    </div>
  )

  const renderRemoteState = (
    <div className="grid gap-5 lg:grid-cols-[minmax(0,0.82fr)_minmax(0,1.18fr)]">
      <div className="rounded-[1.4rem] border border-[color-mix(in_srgb,var(--workbench-divider)_92%,transparent)] bg-[color-mix(in_srgb,var(--surface)_90%,transparent)] p-5">
        <div className="flex items-center gap-3">
          <span className="grid size-11 place-items-center rounded-[1rem] border border-[color-mix(in_srgb,var(--accent)_14%,transparent)] bg-[color-mix(in_srgb,var(--surface)_78%,white_22%)] text-[var(--accent)]">
            <Globe className="size-5" />
          </span>
          <div>
            <div className="text-[0.96rem] font-semibold tracking-[-0.02em] text-[var(--foreground)]">远程模式</div>
            <div className="mt-1 text-[0.72rem] text-[var(--muted)]">按现有远程连接模型配置地址与认证，然后测试连通性。</div>
          </div>
        </div>

        <div className="mt-5 grid gap-3">
          <div className="rounded-[1rem] border border-[color-mix(in_srgb,var(--accent)_12%,transparent)] bg-[color-mix(in_srgb,var(--accent)_5%,transparent)] p-4">
            <div className="text-[0.74rem] font-medium text-[var(--foreground)]">流程说明</div>
            <div className="mt-2 text-[0.72rem] leading-5 text-[var(--muted)]">
              填写远程地址后会自动探测网关；如果识别到 OAuth，就先登录再测试；如果是 Token，则直接填令牌即可。
            </div>
          </div>

          {remoteState.envOverride ? (
            <div className="rounded-[1rem] border border-[color-mix(in_srgb,var(--danger)_18%,transparent)] bg-[var(--danger-soft)] p-4 text-[0.72rem] leading-5 text-[var(--danger)]">
              当前连接由环境变量接管。你仍可查看配置，但运行时可能优先使用外部注入的连接信息。
            </div>
          ) : null}
        </div>
      </div>

      <div className="rounded-[1.4rem] border border-[color-mix(in_srgb,var(--workbench-divider)_92%,transparent)] bg-[var(--workbench-panel-bg)] p-5">
        {remoteLoading ? (
          <div className="flex min-h-[18rem] flex-col items-center justify-center text-center">
            <div className="grid size-16 place-items-center rounded-full border border-[color-mix(in_srgb,var(--accent)_16%,transparent)] bg-[color-mix(in_srgb,var(--accent)_8%,transparent)] text-[var(--accent)]">
              <Loader2 className="size-7 animate-spin" />
            </div>
            <div className="mt-5 text-[1rem] font-semibold text-[var(--foreground)]">正在读取远程配置</div>
          </div>
        ) : (
          <div className="grid gap-4">
            {phase === 'remote_failed' && errorDetail ? (
              <div className="flex items-start gap-3 rounded-[1rem] border border-[color-mix(in_srgb,var(--danger)_18%,transparent)] bg-[var(--danger-soft)] p-4 text-[var(--danger)]">
                <AlertCircle className="mt-0.5 size-5 shrink-0" />
                <div>
                  <div className="text-[0.84rem] font-semibold">远程连接失败</div>
                  <div className="mt-1 text-[0.74rem] leading-5">{errorDetail}</div>
                </div>
              </div>
            ) : null}

            {phase === 'remote_ready' && statusMessage ? (
              <div className="flex items-start gap-3 rounded-[1rem] border border-emerald-500/20 bg-emerald-500/10 p-4 text-emerald-700">
                <Check className="mt-0.5 size-5 shrink-0" />
                <div>
                  <div className="text-[0.84rem] font-semibold">连接成功</div>
                  <div className="mt-1 text-[0.74rem] leading-5">{statusMessage}</div>
                </div>
              </div>
            ) : null}

            <div className="grid gap-2">
              <div className="text-[0.74rem] font-medium text-[var(--foreground)]">远程地址</div>
              <Input
                className="h-10 text-[0.74rem]"
                disabled={remoteState.envOverride || phase === 'remote_testing'}
                onChange={event =>
                  setRemoteState(current => ({
                    ...current,
                    remoteUrl: event.target.value
                  }))
                }
                placeholder="https://gateway.example.com/hermes"
                value={remoteState.remoteUrl}
              />
              <div className="text-[0.68rem] leading-5 text-[var(--muted)]">
                建议填完整的 Hermes 网关地址，应用会自动探测 `/api/status`。
              </div>
            </div>

            <div className="grid gap-2">
              <div className="flex flex-wrap items-center gap-2">
                <div className="text-[0.74rem] font-medium text-[var(--foreground)]">认证方式</div>
                {probeStatusState === 'done' && probeState?.authMode !== 'unknown' ? (
                  <span className="rounded-full bg-[color-mix(in_srgb,var(--accent)_8%,transparent)] px-2 py-0.5 text-[0.64rem] font-medium text-[var(--accent)]">
                    已自动识别
                  </span>
                ) : null}
              </div>
              <div className="flex flex-wrap gap-2">
                <SourceOptionButton
                  active={authMode === 'token'}
                  label="Token"
                  onClick={() => setRemoteState(current => ({ ...current, remoteAuthMode: 'token' }))}
                />
                <SourceOptionButton
                  active={authMode === 'oauth'}
                  label="OAuth"
                  onClick={() => setRemoteState(current => ({ ...current, remoteAuthMode: 'oauth' }))}
                />
              </div>
            </div>

            {shouldProbe ? (
              <div className="rounded-[1rem] border border-[color-mix(in_srgb,var(--workbench-divider)_92%,transparent)] bg-[color-mix(in_srgb,var(--surface)_92%,transparent)] p-4 text-[0.72rem] text-[var(--muted)]">
                {probeStatusState === 'probing' ? (
                  <div className="flex items-center gap-2">
                    <Loader2 className="size-4 animate-spin" />
                    正在探测远程网关...
                  </div>
                ) : probeStatusState === 'done' ? (
                  <div className="grid gap-1">
                    <div className="font-medium text-[var(--foreground)]">探测成功</div>
                    <div>
                      地址：{probeState?.baseUrl}
                      {probeState?.version ? ` · Hermes ${probeState.version}` : ''}
                    </div>
                  </div>
                ) : probeStatusState === 'error' ? (
                  <div className="grid gap-1">
                    <div className="font-medium text-[var(--foreground)]">探测未通过</div>
                    <div>{probeState?.error || '暂时无法访问这个远程地址。'}</div>
                  </div>
                ) : null}
              </div>
            ) : null}

            {authMode === 'token' ? (
              <div className="grid gap-2">
                <div className="text-[0.74rem] font-medium text-[var(--foreground)]">访问令牌</div>
                <Input
                  className="h-10 text-[0.74rem]"
                  disabled={remoteState.envOverride || phase === 'remote_testing'}
                  onChange={event => setRemoteToken(event.target.value)}
                  placeholder={remoteState.remoteTokenSet ? '已保存令牌，可直接测试或输入新令牌覆盖' : '粘贴远程 Hermes Token'}
                  type="password"
                  value={remoteToken}
                />
                {remoteState.remoteTokenSet && !remoteToken.trim() ? (
                  <div className="text-[0.68rem] leading-5 text-[var(--muted)]">
                    当前已保存令牌：{remoteState.remoteTokenPreview || '已保存，可直接测试连接。'}
                  </div>
                ) : null}
              </div>
            ) : (
              <div className="rounded-[1rem] border border-[color-mix(in_srgb,var(--workbench-divider)_92%,transparent)] bg-[color-mix(in_srgb,var(--surface)_92%,transparent)] p-4">
                <div className="flex flex-wrap items-center justify-between gap-3">
                  <div>
                    <div className="text-[0.76rem] font-medium text-[var(--foreground)]">OAuth 登录</div>
                    <div className="mt-1 text-[0.7rem] leading-5 text-[var(--muted)]">
                      {remoteState.remoteOauthConnected ? '已完成远程授权，可以直接测试连接。' : '先完成浏览器授权，再回来测试连接。'}
                    </div>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {remoteState.remoteOauthConnected ? (
                      <Button disabled={signingIn || remoteState.envOverride} onClick={() => void signOutRemote()} size="sm" variant="outline">
                        {signingIn ? <Loader2 className="size-4 animate-spin" /> : null}
                        退出登录
                      </Button>
                    ) : null}
                    <Button disabled={signingIn || remoteState.envOverride || !trimmedRemoteUrl} onClick={() => void signInRemote()} size="sm">
                      {signingIn ? <Loader2 className="size-4 animate-spin" /> : null}
                      {remoteState.remoteOauthConnected ? '重新登录' : '去登录'}
                    </Button>
                  </div>
                </div>
              </div>
            )}

            <div className="flex flex-wrap gap-2 pt-1">
              <Button disabled={phase === 'remote_testing'} onClick={() => void testRemote()}>
                {phase === 'remote_testing' ? <Loader2 className="size-4 animate-spin" /> : <CheckCircle2 className="size-4" />}
                {phase === 'remote_testing' ? '测试中' : phase === 'remote_ready' ? '重新测试' : '测试连接'}
              </Button>
              {phase === 'remote_ready' ? (
                <Button onClick={() => completeStartupGuide()} variant="outline">
                  <Sparkles className="size-4" />
                  立即进入
                </Button>
              ) : null}
            </div>

            {phase === 'remote_ready' ? (
              <p className="text-[0.7rem] leading-5 text-[var(--muted)]">若无操作，将在 1 秒后自动进入应用。</p>
            ) : null}
          </div>
        )}
      </div>
    </div>
  )

  return (
    <StartupSurface
      description="这一步只负责准备 Hermes 连接方式：本地就检查本机 Agent，远程就配置并测试远程网关。"
      eyebrow="Startup Mode Gate"
      headerActions={headerActions}
      title="先选择这次如何进入 Hermes"
    >
      {phase === 'mode_gate' ? renderModeGate : null}
      {(phase === 'local_checking' || phase === 'local_failed' || phase === 'local_install_required' || phase === 'local_installing' || phase === 'local_ready') ? renderLocalState : null}
      {(phase === 'remote_editing' || phase === 'remote_failed' || phase === 'remote_ready' || phase === 'remote_testing') ? renderRemoteState : null}
    </StartupSurface>
  )
}

export default StartupController
