/** @type {import('tailwindcss').Config} */
// Semantic colors map to CSS variables; the active design system swaps the variables (see index.css),
// so every component is theme-agnostic — `bg-surface`, `text-muted`, `rounded-md` look right in all
// three systems and in light/dark, with zero per-theme conditionals in the components.
export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        bg: 'var(--color-bg)',
        surface: 'var(--color-surface)',
        surface2: 'var(--color-surface2)',
        border: 'var(--color-border)',
        text: 'var(--color-text)',
        muted: 'var(--color-text-muted)',
        accent: {
          DEFAULT: 'var(--color-accent)',
          fg: 'var(--color-accent-fg)',
          hover: 'var(--color-accent-hover)',
          soft: 'var(--color-accent-soft)',
        },
        success: { DEFAULT: 'var(--color-success)', bg: 'var(--color-success-bg)' },
        warning: { DEFAULT: 'var(--color-warning)', bg: 'var(--color-warning-bg)' },
        danger: { DEFAULT: 'var(--color-danger)', bg: 'var(--color-danger-bg)' },
        input: 'var(--color-input)',
        'input-border': 'var(--color-input-border)',
        viz: {
          1: 'var(--viz-1)',
          2: 'var(--viz-2)',
          3: 'var(--viz-3)',
          4: 'var(--viz-4)',
          grid: 'var(--viz-grid)',
        },
      },
      borderRadius: {
        sm: 'var(--radius-sm)',
        md: 'var(--radius-md)',
        lg: 'var(--radius-lg)',
      },
      boxShadow: {
        xs: 'var(--shadow-xs)',
        sm: 'var(--shadow-sm)',
        md: 'var(--shadow-md)',
        overlay: 'var(--shadow-overlay)',
        focus: 'var(--shadow-focus)',
        'focus-danger': 'var(--shadow-focus-danger)',
      },
      fontFamily: {
        sans: 'var(--font-body)',
        display: 'var(--font-display)',
        mono: 'var(--font-mono)',
      },
      ringColor: {
        DEFAULT: 'var(--color-ring)',
      },
      ringOffsetColor: {
        DEFAULT: 'var(--color-ring-offset)',
      },
      transitionDuration: {
        fast: 'var(--dur-fast)',
        base: 'var(--dur-base)',
        slow: 'var(--dur-slow)',
      },
      transitionTimingFunction: {
        out: 'var(--ease-out)',
        'in-out': 'var(--ease-in-out)',
      },
      zIndex: {
        sticky: 'var(--z-sticky)',
        drawer: 'var(--z-drawer)',
        overlay: 'var(--z-overlay)',
        dialog: 'var(--z-dialog)',
        toast: 'var(--z-toast)',
        tooltip: 'var(--z-tooltip)',
      },
    },
  },
  plugins: [],
}
