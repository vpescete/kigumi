import type { Theme } from '../contract'

// Swiss — Bold international-typographic ERP for operators who read dense grids all day and want one decisive red signal, not a glowing dashboard.
export const swiss: Theme = {
  id: 'swiss',
  name: 'Swiss',
  author: 'Meshble',
  version: '1.0.0',
  compat: '^0.1',
  defaultMode: 'light',
  fontImports: ['https://fonts.googleapis.com/css2?family=Archivo:wght@400;500;600;700;800&family=Libre+Franklin:wght@400;500;600;700&family=Spline+Sans+Mono:wght@400;500;600&display=swap'],
  fonts: { display: "'Archivo', ui-sans-serif, system-ui, -apple-system, sans-serif", body: "'Libre Franklin', ui-sans-serif, system-ui, -apple-system, sans-serif", mono: "'Spline Sans Mono', ui-monospace, SFMono-Regular, Menlo, monospace" },
  type: {
    display: { stack: 'display', size: '52px', weight: 800, lh: '1.02', tracking: '-0.035em', transform: 'none' },
    h1: { stack: 'display', size: '34px', weight: 700, lh: '1.06', tracking: '-0.025em', transform: 'none' },
    h2: { stack: 'display', size: '24px', weight: 700, lh: '1.12', tracking: '-0.015em', transform: 'none' },
    subtitle: { stack: 'body', size: '16px', weight: 500, lh: '1.4', tracking: '0', transform: 'none' },
    body: { stack: 'body', size: '14px', weight: 400, lh: '1.5', tracking: '0', transform: 'none' },
    label: { stack: 'body', size: '11px', weight: 600, lh: '1.2', tracking: '0.08em', transform: 'uppercase' },
    caption: { stack: 'body', size: '12px', weight: 400, lh: '1.35', tracking: '0.01em', transform: 'none' },
    mono: { stack: 'mono', size: '13px', weight: 500, lh: '1.4', tracking: '-0.01em', transform: 'none' }
  },
  radius: { sm: '0px', md: '1px', lg: '2px' },
  shadow: { sm: "0 0 0 1px rgba(20,20,19,0.06)", md: "0 1px 0 0 rgba(20,20,19,0.10)" },
  density: { row: '38px', control: '34px', fsBase: '14px', space: '8px' },
  color: {
    light: { bg: '#F4F3F0', surface: '#FBFAF8', surface2: '#EDEBE6', border: '#D6D3CC', text: '#161514', textMuted: '#5E5A53', accent: '#E2281C', accentFg: '#FFFFFF', accentHover: '#C42A21', accentSoft: '#FBE6E3', success: '#1F7A4D', successBg: '#E2F0E8', warning: '#9A6510', warningBg: '#F6EBD7', danger: '#C42A21', dangerBg: '#FBE0DD', ring: '#E5392E' },
    dark: { bg: '#121110', surface: '#1A1917', surface2: '#242220', border: '#36332F', text: '#F5F3EF', textMuted: '#A39E95', accent: '#F25147', accentFg: '#1A0605', accentHover: '#FF6E64', accentSoft: '#2E1916', success: '#54C089', successBg: '#16271E', warning: '#D9A441', warningBg: '#2B2310', danger: '#F25147', dangerBg: '#301513', ring: '#F25147' },
  },
}
