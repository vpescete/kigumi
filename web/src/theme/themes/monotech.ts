import type { Theme } from '../contract'

// Mono-Tech — A dense, engineering-grade ERP console where every number is monospaced and amber signals intent.
export const monotech: Theme = {
  id: 'monotech',
  name: 'Mono-Tech',
  author: 'Meshble',
  version: '1.0.0',
  compat: '^0.1',
  defaultMode: 'dark',
  fontImports: ['https://fonts.googleapis.com/css2?family=IBM+Plex+Sans:wght@400;500;600;700&family=IBM+Plex+Mono:wght@400;500;600&display=swap'],
  fonts: { display: "'IBM Plex Sans', ui-sans-serif, system-ui, -apple-system, sans-serif", body: "'IBM Plex Sans', ui-sans-serif, system-ui, -apple-system, sans-serif", mono: "'IBM Plex Mono', ui-monospace, SFMono-Regular, Menlo, monospace" },
  type: {
    display: { stack: 'display', size: '30px', weight: 600, lh: '1.1', tracking: '-0.02em', transform: 'none' },
    h1: { stack: 'display', size: '22px', weight: 600, lh: '1.18', tracking: '-0.015em', transform: 'none' },
    h2: { stack: 'display', size: '17px', weight: 600, lh: '1.25', tracking: '-0.01em', transform: 'none' },
    subtitle: { stack: 'body', size: '14px', weight: 500, lh: '1.4', tracking: '0em', transform: 'none' },
    body: { stack: 'body', size: '13px', weight: 400, lh: '1.5', tracking: '0em', transform: 'none' },
    label: { stack: 'body', size: '11px', weight: 600, lh: '1.3', tracking: '0.06em', transform: 'uppercase' },
    caption: { stack: 'body', size: '11px', weight: 400, lh: '1.35', tracking: '0.01em', transform: 'none' },
    mono: { stack: 'mono', size: '12px', weight: 400, lh: '1.4', tracking: '0em', transform: 'none' }
  },
  radius: { sm: '2px', md: '4px', lg: '6px' },
  shadow: { sm: "0 1px 2px rgba(15,18,22,0.32)", md: "0 4px 12px rgba(15,18,22,0.40)" },
  density: { row: '34px', control: '30px', fsBase: '13px', space: '8px' },
  color: {
    light: { bg: '#EEF1F4', surface: '#FFFFFF', surface2: '#F4F6F8', border: '#D2D8DE', text: '#1A2027', textMuted: '#5A646E', accent: '#9A6B00', accentFg: '#FFFFFF', accentHover: '#855C00', accentSoft: '#FBEFD2', success: '#1F7A40', successBg: '#E0F1E6', warning: '#8A5A00', warningBg: '#FBEFD2', danger: '#B22230', dangerBg: '#FBE3E5', ring: '#9A6B00' },
    dark: { bg: '#0F1318', surface: '#161B22', surface2: '#1D242D', border: '#2C353F', text: '#E5EAF0', textMuted: '#9AA6B2', accent: '#E0A500', accentFg: '#1A1402', accentHover: '#F2B71A', accentSoft: '#332A12', success: '#46C77E', successBg: '#16291F', warning: '#E0A500', warningBg: '#2E2710', danger: '#F0626F', dangerBg: '#311419', ring: '#E0A500' },
  },
}
