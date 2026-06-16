import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'
import type { Mode, Theme } from './theme/contract'
import { getAllThemes, subscribe } from './theme/registry'

export type { Mode }

type Ctx = {
  theme: string
  mode: Mode
  themes: Theme[]
  setTheme: (id: string) => void
  toggleMode: () => void
}
const ThemeCtx = createContext<Ctx | null>(null)

const knownId = (id: string | null, themes: Theme[]): string =>
  id && themes.some((t) => t.id === id) ? id : 'graphite'

export function ThemeProvider({ children }: { children: ReactNode }) {
  // Re-render when the registry changes (a theme added in the Studio / dropped in).
  const [, bump] = useState(0)
  useEffect(() => subscribe(() => bump((n) => n + 1)), [])
  const themes = getAllThemes()

  const [theme, setThemeState] = useState<string>(() => knownId(localStorage.getItem('msh-theme'), getAllThemes()))
  const [mode, setMode] = useState<Mode>(() => {
    const s = localStorage.getItem('msh-mode')
    return s === 'light' || s === 'dark' ? s : 'dark'
  })

  useEffect(() => {
    const el = document.documentElement
    el.dataset.theme = theme
    el.dataset.mode = mode
    localStorage.setItem('msh-theme', theme)
    localStorage.setItem('msh-mode', mode)
  }, [theme, mode])

  const setTheme = (id: string) => {
    setThemeState(id)
    const def = themes.find((t) => t.id === id)?.defaultMode
    if (def) setMode(def)
  }

  return (
    <ThemeCtx.Provider value={{ theme, mode, themes, setTheme, toggleMode: () => setMode(mode === 'dark' ? 'light' : 'dark') }}>
      {children}
    </ThemeCtx.Provider>
  )
}

export function useTheme() {
  const c = useContext(ThemeCtx)
  if (!c) throw new Error('useTheme must be used within ThemeProvider')
  return c
}
