// The THEME CONTRACT — the public, versioned shape a theme must satisfy. A theme is declarative
// DATA, not code: the framework turns it into CSS variables that the (theme-agnostic) components
// read. Community authors target this contract; nothing in the UI depends on a theme's internals.

export type Mode = 'light' | 'dark'

/** Semantic color tokens. Components reference these via Tailwind (`bg-surface`, `text-muted`, …). */
export const COLOR_TOKENS = [
  'bg', 'surface', 'surface2', 'border', 'text', 'textMuted',
  'accent', 'accentFg', 'accentHover', 'accentSoft',
  'success', 'successBg', 'warning', 'warningBg', 'danger', 'dangerBg', 'ring',
] as const
export type ColorToken = (typeof COLOR_TOKENS)[number]

/** Typographic roles. Each maps to a `.t-<role>` class; a theme gives each role its own font. */
export const TYPE_ROLES = ['display', 'h1', 'h2', 'subtitle', 'body', 'label', 'caption', 'mono'] as const
export type TypeRole = (typeof TYPE_ROLES)[number]

export interface RoleSpec {
  stack: 'display' | 'body' | 'mono'
  size: string // e.g. "30px"
  weight: number // 300–800
  lh: string // line-height, e.g. "1.2"
  tracking: string // letter-spacing, e.g. "-0.02em"
  transform?: 'none' | 'uppercase'
}

export type Palette = Record<ColorToken, string>

export interface Theme {
  id: string
  name: string
  author?: string
  /** SemVer of the theme itself. */
  version?: string
  /** Framework compatibility range (mirrors module/framework versioning), e.g. "^0.1". */
  compat?: string
  defaultMode: Mode
  /** Webfont stylesheet hrefs (e.g. Google Fonts). Injected once, deduped. */
  fontImports: string[]
  fonts: { display: string; body: string; mono: string }
  type: Record<TypeRole, RoleSpec>
  radius: { sm: string; md: string; lg: string }
  shadow: { sm: string; md: string }
  density: { row: string; control: string; fsBase: string; space: string }
  color: Record<Mode, Palette>
}

/** camelCase token → CSS custom property name (`textMuted` → `--color-text-muted`). */
export const colorVar = (token: string): string =>
  '--color-' + token.replace(/[A-Z]/g, (m) => '-' + m.toLowerCase())
