import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'

export type ThemeName = 'linear' | 'stripe' | 'notion'
export type Mode = 'light' | 'dark'

/** The three design systems, with their natural default mode and a one-line character. */
export const THEMES: { id: ThemeName; name: string; blurb: string; defaultMode: Mode }[] = [
  { id: 'linear', name: 'Linear', blurb: 'Dark · dense · keyboard-first', defaultMode: 'dark' },
  { id: 'stripe', name: 'Stripe', blurb: 'Light · airy · refined', defaultMode: 'light' },
  { id: 'notion', name: 'Notion', blurb: 'Light · soft · approachable', defaultMode: 'light' },
]

type Ctx = {
  theme: ThemeName
  mode: Mode
  setTheme: (t: ThemeName) => void
  toggleMode: () => void
}
const ThemeCtx = createContext<Ctx | null>(null)

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<ThemeName>(
    () => (localStorage.getItem('msh-theme') as ThemeName) || 'linear',
  )
  const [mode, setMode] = useState<Mode>(() => (localStorage.getItem('msh-mode') as Mode) || 'dark')

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
