// Theme registry: the live set of available themes = built-ins + community drop-ins (/themes/*.json)
// + user-created customs (localStorage). Components subscribe so the switcher updates when a theme is
// added in the Studio or dropped in.

import type { Theme } from './contract'
import { builtinThemes } from './themes'
import { injectThemes } from './css'
import { lintTheme } from './validate'

const CUSTOM_KEY = 'msh-custom-themes'

let custom: Theme[] = loadCustom() // user-authored, persisted
let dropins: Theme[] = [] // loaded from /themes, ephemeral
const listeners = new Set<() => void>()

function loadCustom(): Theme[] {
  try {
    const raw = localStorage.getItem(CUSTOM_KEY)
    if (!raw) return []
    const parsed: unknown = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    // Never trust persisted shape — a hand-edited entry must pass the same lint as a fresh theme.
    return parsed.filter(
      (t): t is Theme =>
        typeof t === 'object' && t !== null && lintTheme(t as Theme).every((l) => l.level !== 'error'),
    )
  } catch {
    return []
  }
}
const persist = () => localStorage.setItem(CUSTOM_KEY, JSON.stringify(custom))

/** Built-ins, then drop-ins, then customs — later entries override an earlier same-id. */
export function getAllThemes(): Theme[] {
  const byId = new Map<string, Theme>()
  for (const t of [...builtinThemes, ...dropins, ...custom]) byId.set(t.id, t)
  return [...byId.values()]
}
export const isBuiltin = (id: string) => builtinThemes.some((t) => t.id === id)
export const isCustom = (id: string) => custom.some((t) => t.id === id)

export function subscribe(fn: () => void): () => void {
  listeners.add(fn)
  return () => listeners.delete(fn)
}
function notify() {
  injectThemes(getAllThemes())
  listeners.forEach((f) => f())
}

/** Adds or replaces a user theme. Rejects on structural errors (returns them). */
export function upsertCustomTheme(t: Theme): { ok: boolean; errors: string[] } {
  const errors = lintTheme(t).filter((l) => l.level === 'error').map((l) => l.message)
  if (errors.length) return { ok: false, errors }
  custom = [...custom.filter((x) => x.id !== t.id), t]
  persist()
  notify()
  return { ok: true, errors: [] }
}

export function removeCustomTheme(id: string): void {
  custom = custom.filter((x) => x.id !== id)
  persist()
  notify()
}

/** Loads JSON themes listed in /themes/index.json (a manifest of filenames). Invalid files skipped. */
export async function loadDropInThemes(): Promise<void> {
  try {
    const res = await fetch('/themes/index.json')
    if (!res.ok) return
    const files: string[] = await res.json()
    const loaded: Theme[] = []
    for (const f of files) {
      try {
        const r = await fetch(`/themes/${f}`)
        if (!r.ok) continue
        const t = (await r.json()) as Theme
        if (lintTheme(t).some((l) => l.level === 'error')) continue
        if (isBuiltin(t.id)) continue // a drop-in must not shadow a built-in id
        loaded.push({ ...t, author: t.author ?? 'community' })
      } catch {
        /* skip a malformed file */
      }
    }
    if (loaded.length) {
      dropins = loaded
      notify()
    }
  } catch {
    /* no manifest present — perfectly fine */
  }
}
