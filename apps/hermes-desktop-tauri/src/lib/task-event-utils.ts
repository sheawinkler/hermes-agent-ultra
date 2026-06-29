import type { TaskEvent } from '@/types/task'

export function eventPayloadText(event: TaskEvent): string {
  const payload = event.payload ?? {}
  const text = payload.text
  if (typeof text === 'string') return text
  const message = payload.message
  if (typeof message === 'string') return message
  return ''
}

export function eventPayloadSteps(event: TaskEvent): string[] {
  const payload = event.payload ?? {}
  const steps = payload.steps
  if (Array.isArray(steps)) {
    return steps.map(step => String(step))
  }
  const plan = payload.plan
  if (Array.isArray(plan)) {
    return plan.map(step => String(step))
  }
  const text = eventPayloadText(event)
  return text
    ? text
        .split('\n')
        .map(line => line.trim())
        .filter(Boolean)
    : []
}

export function eventToolName(event: TaskEvent): string {
  const payload = event.payload ?? {}
  if (typeof payload.tool_name === 'string') return payload.tool_name
  if (typeof payload.name === 'string') return payload.name
  if (event.actor.type === 'tool') return event.actor.tool_name
  return 'tool'
}

export function eventToolArgs(event: TaskEvent): string {
  const payload = event.payload ?? {}
  const args = payload.args ?? payload.arguments
  if (typeof args === 'string') return args
  if (args !== undefined) {
    try {
      return JSON.stringify(args, null, 2)
    } catch {
      return String(args)
    }
  }
  return ''
}

export function eventToolResult(event: TaskEvent): string {
  const payload = event.payload ?? {}
  const result = payload.result ?? payload.output ?? payload.text
  if (typeof result === 'string') return result
  if (result !== undefined) {
    try {
      return JSON.stringify(result, null, 2)
    } catch {
      return String(result)
    }
  }
  return ''
}

export function eventApprovalSummary(event: TaskEvent): string {
  const payload = event.payload ?? {}
  if (typeof payload.summary === 'string') return payload.summary
  if (typeof payload.reason === 'string') return payload.reason
  if (typeof payload.description === 'string') return payload.description
  return event.toc_label ?? 'Approval required'
}

export function eventArtifactName(event: TaskEvent): string {
  const payload = event.payload ?? {}
  if (typeof payload.filename === 'string') return payload.filename
  if (typeof payload.name === 'string') return payload.name
  return event.toc_label ?? 'artifact'
}

export function outlineItemsFromEvents(events: TaskEvent[]): { id: string; label: string; depth?: number }[] {
  return events
    .filter(event => event.toc_label || event.anchor_slug)
    .map(event => ({
      id: event.anchor_slug,
      label: event.toc_label ?? event.kind,
      depth: event.kind === 'subagent_spawn' ? 1 : 0
    }))
}

export function branchIdsFromEvents(events: TaskEvent[]): string[] {
  const ids = new Set<string>()
  for (const event of events) {
    const payload = event.payload ?? {}
    const subTask = payload.sub_task_id ?? payload.child_task_id
    if (typeof subTask === 'string' && subTask.trim()) {
      ids.add(subTask)
    }
  }
  return [...ids]
}

export function progressFromEvents(events: TaskEvent[]): number {
  if (events.length === 0) return 0
  const terminal = events.filter(event =>
    ['message', 'artifact', 'error', 'checkpoint'].includes(event.kind)
  ).length
  return Math.min(100, Math.round((terminal / events.length) * 100))
}

export function minimapColorForKind(kind: TaskEvent['kind']): string {
  switch (kind) {
    case 'instruction':
    case 'message':
      return '#3b82f6'
    case 'plan':
      return '#0ea5e9'
    case 'thinking':
      return '#94a3b8'
    case 'tool_call':
    case 'tool_result':
      return '#8b5cf6'
    case 'artifact':
      return '#10b981'
    case 'approval_request':
    case 'approval_response':
      return '#f59e0b'
    case 'error':
      return '#ef4444'
    case 'checkpoint':
      return '#64748b'
    case 'subagent_spawn':
      return '#a855f7'
    case 'system':
    default:
      return '#cbd5e1'
  }
}
