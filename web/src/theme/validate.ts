// Theme validation: structural (all tokens/roles present) + a WCAG contrast linter (the same
// >= 4.5:1 rule the design QA used). Authors get warnings before a theme ships.

import { COLOR_TOKENS, TYPE_ROLES, type Mode, type Palette, type Theme } from './contract'

/** Parses #rgb / #rrggbb to [r,g,b] 0–255, or null if unparseable. */
export function hexToRgb(hex: string): [number, number, number] | null {
  const m = hex.trim().replace('#', '')
  if (m.length === 3) {
    const r = parseInt(m[0] + m[0], 16)
    const g = parseInt(m[1] + m[1], 16)
    const b = parseInt(m[2] + m[2], 16)
    return [r, g, b]
  }
  if (m.length === 6 || m.length === 8) {
    const r = parseInt(m.slice(0, 2), 16)
    const g = parseInt(m.slice(2, 4), 16)
    const b = parseInt(m.slice(4, 6), 16)
    if ([r, g, b].some(Number.isNaN)) return null
    return [r, g, b]
  }
  return null
}

const channel = (c: number) => {
  const s = c / 255
  return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4)
}

export function relLuminance(hex: string): number | null {
  const rgb = hexToRgb(hex)
  if (!rgb) return null
  const [r, g, b] = rgb
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/** WCAG contrast ratio (1–21), or null if either color is unparseable. */
export function contrast(a: string, b: string): number | null {
  const la = relLuminance(a)
  const lb = relLuminance(b)
  if (la === null || lb === null) return null
  const hi = Math.max(la, lb)
  const lo = Math.min(la, lb)
  return (hi + 0.05) / (lo + 0.05)
}

export type Lint = { level: 'error' | 'warn'; message: string }

// The text/background pairs that must read clearly.
const PAIRS: { fg: keyof Palette; bg: keyof Palette; label: string }[] = [
  { fg: 'text', bg: 'bg', label: 'text on bg' },
  { fg: 'text', bg: 'surface', label: 'text on surface' },
  { fg: 'textMuted', bg: 'surface', label: 'muted text on surface' },
  { fg: 'accentFg', bg: 'accent', label: 'label on accent' },
]

// Every theme value is injected verbatim into a <style>; restrict each to a safe shape so a (possibly
// untrusted) drop-in / localStorage theme cannot break out of the declaration and inject arbitrary
// CSS (e.g. `@import`, `url(...)` exfiltration, or extra rules).
const SAFE_COLOR = /^#([0-9a-f]{3}|[0-9a-f]{6})$|^(rgb|rgba|hsl|hsla)\([0-9.,%\s/-]+\)$/i
// Disallow anything that could terminate a declaration or open a new rule / fetch.
const breakout = (v: string): boolean => /[;{}<>@\\]|\/\*|url\s*\(|expression\s*\(|image-set\s*\(/i.test(v)

// Allowed hosts for webfont stylesheet imports.
const FONT_HOSTS = new Set([
  'fonts.googleapis.com',
  'fonts.gstatic.com',
  'rsms.me',
  'use.typekit.net',
  'api.fontshare.com',
])

/** True iff `href` is an https URL on an allowlisted font CDN (no whitespace). */
export function isSafeFontHref(href: unknown): boolean {
  if (typeof href !== 'string' || /\s/.test(href)) return false
  try {
    const u = new URL(href)
    return u.protocol === 'https:' && FONT_HOSTS.has(u.hostname)
  } catch {
    return false
  }
}

export function lintTheme(t: Theme): Lint[] {
  const out: Lint[] = []
  const err = (message: string) => out.push({ level: 'error', message })

  if (!t.id || !/^[a-z0-9-]+$/.test(t.id)) err(`id must be kebab-case (got "${t.id}")`)
  for (const f of ['name', 'author', 'version'] as const) {
    const v = t[f]
    if (v != null && (typeof v !== 'string' || v.length > 80 || breakout(v))) err(`${f} is invalid`)
  }

  // fontImports become <link rel=stylesheet href> — restrict to https + known font CDNs so a theme
  // cannot load an arbitrary stylesheet (CSS exfiltration / tracking).
  if (!Array.isArray(t.fontImports)) {
    err('fontImports must be an array')
  } else {
    for (const href of t.fontImports) {
      if (!isSafeFontHref(href)) err(`fontImports entry is not an allowed https font-CDN URL: ${href}`)
    }
  }

  // String fields injected as CSS values — reject breakout characters.
  const injected: [string, unknown][] = [
    ['fonts.display', t.fonts?.display], ['fonts.body', t.fonts?.body], ['fonts.mono', t.fonts?.mono],
    ['radius.sm', t.radius?.sm], ['radius.md', t.radius?.md], ['radius.lg', t.radius?.lg],
    ['shadow.sm', t.shadow?.sm], ['shadow.md', t.shadow?.md],
    ['density.row', t.density?.row], ['density.control', t.density?.control],
    ['density.fsBase', t.density?.fsBase], ['density.space', t.density?.space],
  ]
  for (const r of TYPE_ROLES) {
    const role = t.type?.[r]
    if (!role) {
      err(`missing type role "${r}"`)
      continue
    }
    if (!['display', 'body', 'mono'].includes(role.stack)) err(`type.${r}.stack invalid`)
    if (typeof role.weight !== 'number') err(`type.${r}.weight must be a number`)
    if (role.transform && !['none', 'uppercase'].includes(role.transform)) err(`type.${r}.transform invalid`)
    injected.push([`type.${r}.size`, role.size], [`type.${r}.lh`, role.lh], [`type.${r}.tracking`, role.tracking])
  }
  for (const [name, v] of injected) {
    if (typeof v !== 'string') err(`missing/invalid ${name}`)
    else if (breakout(v)) err(`${name} contains characters unsafe to inject as CSS`)
  }

  for (const mode of ['light', 'dark'] as Mode[]) {
    const pal = t.color?.[mode]
    if (!pal) {
      err(`missing "${mode}" palette`)
      continue
    }
    for (const tok of COLOR_TOKENS) {
      if (!pal[tok]) err(`${mode}: missing color "${tok}"`)
      else if (!SAFE_COLOR.test(pal[tok].trim())) err(`${mode}: "${tok}" is not a valid hex/rgb/hsl color`)
    }
    for (const p of PAIRS) {
      const ratio = contrast(pal[p.fg], pal[p.bg])
      if (ratio === null) continue
      if (ratio < 4.5)
        out.push({ level: 'warn', message: `${mode}: ${p.label} contrast ${ratio.toFixed(2)}:1 (< 4.5:1)` })
    }
  }
  return out
}

/** Convenience for the Studio's per-pair badges. */
export function pairRatios(pal: Palette): { label: string; ratio: number; pass: boolean }[] {
  return PAIRS.map((p) => {
    const ratio = contrast(pal[p.fg], pal[p.bg]) ?? 0
    return { label: p.label, ratio, pass: ratio >= 4.5 }
  })
}
