import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'

export type ThemeName = 'graphite' | 'editorial' | 'swiss' | 'humanist' | 'monotech'
export type Mode = 'light' | 'dark'

/** The five design systems, with their natural default mode and a one-line character. */
export const THEMES: { id: ThemeName; name: string; blurb: string; defaultMode: Mode }[] = [
  { id: 'graphite', name: 'Graphite', blurb: 'Dark dev-console · cyan · dense', defaultMode: 'dark' },
  { id: 'editorial', name: 'Editorial', blurb: 'Warm serif · terracotta · airy', defaultMode: 'light' },
  { id: 'swiss', name: 'Swiss', blurb: 'Bold grid · signal red · flat', defaultMode: 'light' },
  { id: 'humanist', name: 'Verdigris', blurb: 'Friendly · emerald · rounded', defaultMode: 'light' },
  { id: 'monotech', name: 'Mono-Tech', blurb: 'Ops console · amber · mono', defaultMode: 'dark' },
]

const IDS = THEMES.map((t) => t.id)
const isTheme = (v: string | null): v is ThemeName => !!v && (IDS as string[]).includes(v)

type Ctx = {
  theme: ThemeName
  mode: Mode
  setTheme: (t: ThemeName) => void
  toggleMode: () => void
}
const ThemeCtx = createContext<Ctx | null>(null)

export function ThemeProvider({ children }: { children: ReactNode }) {
  // Validate persisted values — a theme id from an earlier build would leave the app uncolored.
  const [theme, setThemeState] = useState<ThemeName>(() => {
    const saved = localStorage.getItem('msh-theme')
    return isTheme(saved) ? saved : 'graphite'
  })
  const [mode, setMode] = useState<Mode>(() => {
    const saved = localStorage.getItem('msh-mode')
    return saved === 'light' || saved === 'dark' ? saved : 'dark'
  })

  useEffect(() => {
    const el = document.documentElement
    el.dataset.theme = theme
    el.dataset.mode = mode
    localStorage.setItem('msh-theme', theme)
    localStorage.setItem('msh-mode', mode)
  }, [theme, mode])

  // Switching design system jumps to that system's natural default mode.
  const setTheme = (t: ThemeName) => {
    setThemeState(t)
    setMode(THEMES.find((x) => x.id === t)?.defaultMode ?? 'light')
  }

  return (
    <ThemeCtx.Provider
      value={{ theme, mode, setTheme, toggleMode: () => setMode(mode === 'dark' ? 'light' : 'dark') }}
    >
      {children}
    </ThemeCtx.Provider>
  )
}

export function useTheme() {
  const c = useContext(ThemeCtx)
  if (!c) throw new Error('useTheme must be used within ThemeProvider')
  return c
}
