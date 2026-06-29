import { QueryClientProvider } from '@tanstack/react-query'
import { useEffect } from 'react'
import { HashRouter, Route, Routes } from 'react-router-dom'

import { ErrorBoundary } from '@/components/error-boundary'
import { I18nProvider } from '@/i18n'
import { queryClient } from '@/lib/query-client'
import { ThemeProvider } from '@/themes'
import { initTelemetry } from '@/telemetry'

import DesktopController from './app/index'
import HeroDemoView from './app/hero-demo'
import TerraApp from './app/terra'

export default function App() {
  useEffect(() => {
    initTelemetry()
  }, [])

  return (
    <QueryClientProvider client={queryClient}>
      <I18nProvider>
        <ThemeProvider>
          <HashRouter>
            <ErrorBoundary>
              <Routes>
                <Route element={<HeroDemoView />} path="/hero-demo" />
                <Route element={<TerraApp />} path="/terra/*" />
                <Route element={<DesktopController />} path="*" />
              </Routes>
            </ErrorBoundary>
          </HashRouter>
        </ThemeProvider>
      </I18nProvider>
    </QueryClientProvider>
  )
}
