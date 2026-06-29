#!/usr/bin/env node
import { readFileSync, writeFileSync, cpSync, existsSync, mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const variantId = process.argv[2] ?? 'default'
const variantPath = join(root, 'variants', variantId, 'variant.json')

if (!existsSync(variantPath)) {
  console.error(`Variant not found: ${variantPath}`)
  process.exit(1)
}

const variant = JSON.parse(readFileSync(variantPath, 'utf8'))
const brandingTs = join(root, 'src', 'branding.ts')
writeFileSync(
  brandingTs,
  `export const BRAND_NAME = '${variant.brandName}'\nexport const BRAND_SHORT = '${variant.brandName}'\nexport const BRAND_TAGLINE = ''\nexport const BRAND_URL = ''\nexport const BRAND_SUPPORT_EMAIL = '${variant.supportEmail ?? ''}'\n`
)

const tauriConfOut = join(root, 'src-tauri', `tauri.conf.${variantId}.json`)
if (existsSync(join(root, 'src-tauri', 'tauri.conf.json'))) {
  cpSync(join(root, 'src-tauri', 'tauri.conf.json'), tauriConfOut)
}

const iconsSrc = join(root, 'variants', variantId, 'icons')
const iconsDst = join(root, 'src-tauri', 'icons')
if (existsSync(iconsSrc)) {
  mkdirSync(iconsDst, { recursive: true })
  cpSync(iconsSrc, iconsDst, { recursive: true })
}

console.log(`Applied variant: ${variantId}`)
