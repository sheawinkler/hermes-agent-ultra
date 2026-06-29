import { createContext, useContext, useMemo, useState, type ReactNode } from 'react'

export type UiMode = 'standard' | 'developer'

interface UiModeContextValue {
  mode: UiMode
  setMode: (mode: UiMode) => void
  locked: boolean
}

const UiModeContext = createContext<UiModeContextValue | null>(null)

interface UiModeProviderProps {
  children: ReactNode
  initialMode?: UiMode
  locked?: boolean
}

export function UiModeProvider({ children, initialMode = 'standard', locked = false }: UiModeProviderProps) {
  const [mode, setModeState] = useState<UiMode>(initialMode)
  const setMode = (next: UiMode) => {
    if (!locked) setModeState(next)
  }
  const value = useMemo(() => ({ mode, setMode, locked }), [mode, locked])
  return <UiModeContext.Provider value={value}>{children}</UiModeContext.Provider>
}

export function useUiMode() {
  const ctx = useContext(UiModeContext)
  if (!ctx) throw new Error('useUiMode requires UiModeProvider')
  return ctx
}
