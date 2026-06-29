import { useCallback } from 'react'

import { useI18n } from '@/i18n'
import { getTerraCatalog, resolveTerraString } from '@/i18n/terra-locales'

export type TranslationDomain = 'app' | 'task' | 'vertical' | 'composer' | 'settings' | 'billing' | 'auth'

export type DomainTranslator = (key: string, fallback?: string) => string

export function useT(domain?: TranslationDomain): DomainTranslator {
  const { locale } = useI18n()
  const catalog = getTerraCatalog(locale)

  return useCallback(
    (key: string, fallback?: string) => {
      const fullKey = domain ? `${domain}.${key}` : key
      return resolveTerraString(catalog, fullKey) ?? fallback ?? fullKey
    },
    [catalog, domain]
  )
}

export function useTDomain(domain: TranslationDomain): DomainTranslator {
  return useT(domain)
}
