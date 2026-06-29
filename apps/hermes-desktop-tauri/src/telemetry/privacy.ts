const ANON_ID_KEY = 'terra.telemetry.anonymous_user_id'

export function getAnonymousUserId(): string {
  const existing = localStorage.getItem(ANON_ID_KEY)
  if (existing) return existing
  const id = crypto.randomUUID()
  localStorage.setItem(ANON_ID_KEY, id)
  return id
}

export function setTelemetryOptOut(optOut: boolean) {
  ;(window as { __TERRA_TELEMETRY_OPT_OUT__?: boolean }).__TERRA_TELEMETRY_OPT_OUT__ = optOut
}

export function isTelemetryOptOut(): boolean {
  return Boolean((window as { __TERRA_TELEMETRY_OPT_OUT__?: boolean }).__TERRA_TELEMETRY_OPT_OUT__)
}
