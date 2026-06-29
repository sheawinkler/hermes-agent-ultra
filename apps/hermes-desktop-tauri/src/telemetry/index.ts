import { fireTelemetryEvent, type TelemetryEventName, type TelemetryEventPayload } from './events'

export function initTelemetry() {
  fireTelemetryEvent('app_start')
}

export { fireTelemetryEvent, type TelemetryEventName, type TelemetryEventPayload }
