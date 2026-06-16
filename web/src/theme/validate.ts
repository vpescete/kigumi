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

// Colors are injected verbatim into a <style>; restrict them to safe formats so a (possibly
// untrusted) drop-in theme cannot break out of the declaration and inject arbitrary CSS.
const SAFE_COLOR = /^#([0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$|^(rgb|rgba|hsl|hsla)\([0-9.,%\s/-]+\)$/i

export function lintTheme(t: Theme): Lint[] {
  const out: Lint[] = []
  if (!t.id || !/^[a-z0-9-]+$/.test(t.id)) out.push({ level: 'error', message: `id must be kebab-case (got "${t.id}")` })
  for (const r of TYPE_ROLES) if (!t.type?.[r]) out.push({ level: 'error', message: `missing type role "${r}"` })
  for (const mode of ['light', 'dark'] as Mode[]) {
    const pal = t.color?.[mode]
    if (!pal) {
      out.push({ level: 'error', message: `missing "${mode}" palette` })
      continue
    }
    for (const tok of COLOR_TOKENS) {
      if (!pal[tok]) out.push({ level: 'error', message: `${mode}: missing color "${tok}"` })
      else if (!SAFE_COLOR.test(pal[tok].trim()))
        out.push({ level: 'error', message: `${mode}: "${tok}" is not a valid hex/rgb/hsl color` })
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
