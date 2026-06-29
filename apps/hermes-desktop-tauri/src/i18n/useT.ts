import { useI18n } from '@/i18n'

export type TranslationDomain = 'app' | 'task' | 'vertical' | 'composer' | 'settings' | 'billing' | 'auth'

export function useT(_domain?: TranslationDomain) {
  const { t } = useI18n()
  return t
}
