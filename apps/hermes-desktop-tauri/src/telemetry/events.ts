export type TelemetryEventName =
  | 'app_start'
  | 'task_created'
  | 'task_completed'
  | 'approval_requested'
  | 'model_lang_combo_quality'

export interface TelemetryEventPayload {
  [key: string]: string | number | boolean | undefined
}

export function fireTelemetryEvent(name: TelemetryEventName, payload: TelemetryEventPayload = {}) {
  if (typeof window !== 'undefined' && (window as { __TERRA_TELEMETRY_OPT_OUT__?: boolean }).__TERRA_TELEMETRY_OPT_OUT__) {
    return
  }
  console.debug('[telemetry]', name, payload)
}
