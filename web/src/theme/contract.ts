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

/** Data-viz hues (sparklines, mini-charts). Per-mode like colors; `viz1` doubles as the primary trend. */
export const VIZ_TOKENS = ['viz1', 'viz2', 'viz3', 'viz4', 'vizGrid'] as const
export type VizToken = (typeof VIZ_TOKENS)[number]
export type VizPalette = Record<VizToken, string>

/** Motion scale — durations + easings. Theme-agnostic by default (a console feels the same speed in
 * every palette); a theme MAY override. Components use the `--dur-*` / `--ease-*` CSS variables. */
export interface Motion {
  fast: string
  base: string
  slow: string
  easeOut: string
  easeInOut: string
}

export const MOTION_DEFAULTS: Motion = {
  fast: '90ms',
  base: '140ms',
  slow: '220ms',
  easeOut: 'cubic-bezier(0.16, 1, 0.3, 1)',
  easeInOut: 'cubic-bezier(0.4, 0, 0.2, 1)',
}

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
  /** `overlay` is the shadow used by dialogs/popovers; defaults to `md` so a theme need not set it. */
  shadow: { sm: string; md: string; overlay?: string }
  density: { row: string; control: string; fsBase: string; space: string }
  /** Optional motion override; when absent, MOTION_DEFAULTS are emitted. */
  motion?: Motion
  /** Optional data-viz palette; when absent, css.ts derives one from the color palette. */
  viz?: Record<Mode, VizPalette>
  color: Record<Mode, Palette>
}

/** camelCase token → CSS custom property name (`textMuted` → `--color-text-muted`). */
export const colorVar = (token: string): string =>
  '--color-' + token.replace(/[A-Z]/g, (m) => '-' + m.toLowerCase())

/** viz token → CSS custom property (`viz1` → `--viz-1`, `vizGrid` → `--viz-grid`). */
export const vizVar: Record<VizToken, string> = {
  viz1: '--viz-1',
  viz2: '--viz-2',
  viz3: '--viz-3',
  viz4: '--viz-4',
  vizGrid: '--viz-grid',
}
