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
        },
        success: { DEFAULT: 'var(--color-success)', bg: 'var(--color-success-bg)' },
        warning: { DEFAULT: 'var(--color-warning)', bg: 'var(--color-warning-bg)' },
        danger: { DEFAULT: 'var(--color-danger)', bg: 'var(--color-danger-bg)' },
      },
      borderRadius: {
        sm: 'var(--radius-sm)',
        md: 'var(--radius-md)',
        lg: 'var(--radius-lg)',
      },
      boxShadow: {
        sm: 'var(--shadow-sm)',
        md: 'var(--shadow-md)',
      },
      fontFamily: {
        sans: 'var(--font-sans)',
        mono: 'var(--font-mono)',
      },
      ringColor: {
        DEFAULT: 'var(--color-ring)',
      },
    },
  },
  plugins: [],
}
