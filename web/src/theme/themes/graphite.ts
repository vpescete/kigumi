import type { Theme } from '../contract'

// Graphite — A refined dark developer-tool console for engineers who live in the keyboard — dense, quiet, and precise.
export const graphite: Theme = {
  id: 'graphite',
  name: 'Graphite',
  author: 'Kigumi',
  version: '1.0.0',
  compat: '^0.1',
  defaultMode: 'dark',
  fontImports: ['https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@400;500;600;700&family=Hanken+Grotesk:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500;600&display=swap'],
  fonts: { display: "'Space Grotesk', ui-sans-serif, system-ui, -apple-system, sans-serif", body: "'Hanken Grotesk', ui-sans-serif, system-ui, -apple-system, sans-serif", mono: "'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, monospace" },
  type: {
    display: { stack: 'display', size: '40px', weight: 600, lh: '1.05', tracking: '-0.03em', transform: 'none' },
    h1: { stack: 'display', size: '27px', weight: 600, lh: '1.12', tracking: '-0.02em', transform: 'none' },
    h2: { stack: 'display', size: '19px', weight: 600, lh: '1.2', tracking: '-0.01em', transform: 'none' },
    subtitle: { stack: 'body', size: '15px', weight: 500, lh: '1.4', tracking: '0', transform: 'none' },
    body: { stack: 'body', size: '13px', weight: 400, lh: '1.5', tracking: '0', transform: 'none' },
    label: { stack: 'body', size: '11px', weight: 600, lh: '1.3', tracking: '0.07em', transform: 'uppercase' },
    caption: { stack: 'body', size: '12px', weight: 400, lh: '1.35', tracking: '0.01em', transform: 'none' },
    mono: { stack: 'mono', size: '12px', weight: 450, lh: '1.45', tracking: '0', transform: 'none' }
  },
  radius: { sm: '4px', md: '6px', lg: '10px' },
  shadow: { sm: "0 1px 3px 0 rgba(3, 6, 9, 0.5), 0 1px 2px -1px rgba(3, 6, 9, 0.5)", md: "0 4px 16px -6px rgba(6, 9, 12, 0.7)", overlay: "0 12px 40px -10px rgba(3, 6, 9, 0.85)" },
  density: { row: '35px', control: '32px', fsBase: '13px', space: '8px' },
  viz: {
    light: { viz1: '#0D8092', viz2: '#1E7A52', viz3: '#9A6614', viz4: '#5A5FCF', vizGrid: '#D8DBDF' },
    dark: { viz1: '#22B8CF', viz2: '#3DD68C', viz3: '#E0A93B', viz4: '#7B8CFF', vizGrid: '#272D34' },
  },
  color: {
    light: { bg: '#F4F5F6', surface: '#FBFBFC', surface2: '#EDEFF1', border: '#D8DBDF', text: '#16191D', textMuted: '#5B636C', accent: '#0D8092', accentFg: '#FFFFFF', accentHover: '#0B7686', accentSoft: '#DEEFF2', success: '#1E7A52', successBg: '#DBEFE4', warning: '#9A6614', warningBg: '#F6E9D2', danger: '#BE3A37', dangerBg: '#F7DEDD', ring: '#0E8FA3' },
    dark: { bg: '#0D1014', surface: '#14181D', surface2: '#1B2026', border: '#272D34', text: '#E5E9ED', textMuted: '#8B949E', accent: '#22B8CF', accentFg: '#06181C', accentHover: '#46C8DB', accentSoft: '#13313A', success: '#3DD68C', successBg: '#102A20', warning: '#E0A93B', warningBg: '#2C2310', danger: '#F0726E', dangerBg: '#2E1614', ring: '#22B8CF' },
  },
}
