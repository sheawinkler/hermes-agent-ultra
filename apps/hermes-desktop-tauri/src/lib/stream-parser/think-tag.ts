export type ThinkParseState = 'idle' | 'in_tag' | 'done'

export interface ThinkParseResult {
  visible: string
  thinking: string
  state: ThinkParseState
}

const OPEN = '<think>'
const CLOSE = '</think>'

export function parseThinkChunk(buffer: string, chunk: string): ThinkParseResult {
  const combined = buffer + chunk
  const openIdx = combined.indexOf(OPEN)
  const closeIdx = combined.indexOf(CLOSE)

  if (openIdx === -1) {
    return { visible: combined, thinking: '', state: 'idle' }
  }
  if (closeIdx === -1) {
    return {
      visible: combined.slice(0, openIdx),
      thinking: combined.slice(openIdx + OPEN.length),
      state: 'in_tag',
    }
  }

  return {
    visible: combined.slice(0, openIdx) + combined.slice(closeIdx + CLOSE.length),
    thinking: combined.slice(openIdx + OPEN.length, closeIdx),
    state: 'done',
  }
}
