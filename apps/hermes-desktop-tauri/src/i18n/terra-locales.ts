import type { Locale } from './types'

import en from './locales/en.json'
import ja from './locales/ja.json'
import zhCN from './locales/zh-CN.json'
import zhHant from './locales/zh-Hant.json'

export type TerraLocaleKey = 'en' | 'zh-CN' | 'zh-Hant' | 'ja'

export type TerraCatalog = typeof en

const LOCALE_MAP: Record<Locale, TerraLocaleKey> = {
  en: 'en',
  zh: 'zh-CN',
  'zh-hant': 'zh-Hant',
  ja: 'ja'
}

export const TERRA_LOCALES: Record<TerraLocaleKey, TerraCatalog> = {
  en,
  'zh-CN': zhCN,
  'zh-Hant': zhHant,
  ja
}

export function terraLocaleFor(locale: Locale): TerraLocaleKey {
  return LOCALE_MAP[locale]
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function resolveTerraString(catalog: TerraCatalog, key: string): string | undefined {
  const value = key.split('.').reduce<unknown>((current, part) => {
    if (!isRecord(current)) {
      return undefined
    }
    return current[part]
  }, catalog)

  return typeof value === 'string' ? value : undefined
}

export function getTerraCatalog(locale: Locale): TerraCatalog {
  return TERRA_LOCALES[terraLocaleFor(locale)]
}
