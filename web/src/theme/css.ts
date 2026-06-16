// Turns a Theme (data) into CSS variables and injects them at runtime. This is the single mechanism
// for BOTH built-in and community themes — there is no static per-theme CSS, so a dropped-in theme
// behaves identically to a shipped one.

import { colorVar, COLOR_TOKENS, TYPE_ROLES, type Mode, type Theme } from './contract'
import { isSafeFontHref } from './validate'

/** Generates the `[data-theme='id']` (+ light/dark) CSS block for one theme. */
export function themeToCss(t: Theme): string {
  const sel = `[data-theme='${t.id}']`
  const lines: string[] = []
  lines.push(`${sel} {`)
  lines.push(`  --font-display: ${t.fonts.display};`)
  lines.push(`  --font-body: ${t.fonts.body};`)
  lines.push(`  --font-mono: ${t.fonts.mono};`)
  lines.push(`  --radius-sm: ${t.radius.sm}; --radius-md: ${t.radius.md}; --radius-lg: ${t.radius.lg};`)
  lines.push(`  --shadow-sm: ${t.shadow.sm}; --shadow-md: ${t.shadow.md};`)
  lines.push(`  --density-row: ${t.density.row}; --control-h: ${t.density.control};`)
  lines.push(`  --fs-base: ${t.density.fsBase}; --space: ${t.density.space};`)
  for (const r of TYPE_ROLES) {
    const role = t.type[r]
    lines.push(`  --t-${r}: ${role.weight} ${role.size}/${role.lh} var(--font-${role.stack});`)
    lines.push(`  --tr-${r}: ${role.tracking || '0'};`)
    if (r === 'label') lines.push(`  --tt-label: ${role.transform || 'uppercase'};`)
  }
  lines.push('}')
  for (const mode of ['light', 'dark'] as Mode[]) {
    lines.push(`${sel}[data-mode='${mode}'] {`)
    for (const tok of COLOR_TOKENS) lines.push(`  ${colorVar(tok)}: ${t.color[mode][tok]};`)
    lines.push('}')
  }
  return lines.join('\n')
}

/** Appends a webfont <link> for any href not already on the page (deduped). */
export function ensureFontLinks(themes: Theme[]): void {
  const have = new Set(Array.from(document.querySelectorAll('link[rel="stylesheet"]')).map((l) => (l as HTMLLinkElement).href))
  const seen = new Set<string>()
  for (const t of themes) {
    for (const href of t.fontImports) {
      if (seen.has(href)) continue
      seen.add(href)
      if (!isSafeFontHref(href)) continue // defense-in-depth: never link a non-allowlisted stylesheet
      // `have` holds absolute hrefs; compare loosely so we don't double-add.
      if ([...have].some((h) => h === href || h.endsWith(href))) continue
      const link = document.createElement('link')
      link.rel = 'stylesheet'
      link.href = href
      document.head.appendChild(link)
    }
  }
}

/** Injects/replaces the single <style> holding every registered theme's variables. */
export function injectThemes(themes: Theme[]): void {
  ensureFontLinks(themes)
  let style = document.getElementById('msh-themes') as HTMLStyleElement | null
  if (!style) {
    style = document.createElement('style')
    style.id = 'msh-themes'
    document.head.appendChild(style)
  }
  style.textContent = themes.map(themeToCss).join('\n\n')
}

/** Injects/updates a single theme's CSS in isolation (used by the Theme Studio draft preview). */
export function injectOne(t: Theme): void {
  ensureFontLinks([t])
  const id = `msh-theme-${t.id}`
  let style = document.getElementById(id) as HTMLStyleElement | null
  if (!style) {
    style = document.createElement('style')
    style.id = id
    document.head.appendChild(style)
  }
  style.textContent = themeToCss(t)
}
