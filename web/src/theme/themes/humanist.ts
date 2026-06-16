import type { Theme } from '../contract'

// Verdigris — Warm, friendly, human admin UI for people who'd rather not feel like they're using enterprise software.
export const humanist: Theme = {
  id: 'humanist',
  name: 'Verdigris',
  author: 'Meshble',
  version: '1.0.0',
  compat: '^0.1',
  defaultMode: 'light',
  fontImports: ['https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wght@12..96,400;12..96,500;12..96,600;12..96,700;12..96,800&family=DM+Sans:wght@400;500;600;700&family=DM+Mono:wght@400;500&display=swap'],
  fonts: { display: "'Bricolage Grotesque', ui-sans-serif, system-ui, -apple-system, sans-serif", body: "'DM Sans', ui-sans-serif, system-ui, -apple-system, sans-serif", mono: "'DM Mono', ui-monospace, SFMono-Regular, Menlo, monospace" },
  type: {
    display: { stack: 'display', size: '44px', weight: 600, lh: '1.05', tracking: '-0.03em', transform: 'none' },
    h1: { stack: 'display', size: '30px', weight: 600, lh: '1.12', tracking: '-0.02em', transform: 'none' },
    h2: { stack: 'display', size: '22px', weight: 600, lh: '1.2', tracking: '-0.01em', transform: 'none' },
    subtitle: { stack: 'body', size: '17px', weight: 500, lh: '1.4', tracking: '0', transform: 'none' },
    body: { stack: 'body', size: '15px', weight: 400, lh: '1.55', tracking: '0', transform: 'none' },
    label: { stack: 'body', size: '12px', weight: 600, lh: '1.3', tracking: '0.07em', transform: 'uppercase' },
    caption: { stack: 'body', size: '13px', weight: 400, lh: '1.4', tracking: '0.005em', transform: 'none' },
    mono: { stack: 'mono', size: '13px', weight: 400, lh: '1.45', tracking: '0', transform: 'none' }
  },
  radius: { sm: '8px', md: '12px', lg: '16px' },
  shadow: { sm: "0 1px 2px rgba(60, 50, 35, 0.05), 0 1px 1px rgba(60, 50, 35, 0.04)", md: "0 4px 16px rgba(60, 50, 35, 0.08), 0 2px 6px rgba(60, 50, 35, 0.05)" },
  density: { row: '48px', control: '40px', fsBase: '15px', space: '8px' },
  color: {
    light: { bg: '#F7F4EF', surface: '#FFFDF9', surface2: '#F1ECE3', border: '#E4DCCF', text: '#2A2620', textMuted: '#736B5E', accent: '#0C855C', accentFg: '#FFFFFF', accentHover: '#0B8559', accentSoft: '#E2F2EA', success: '#0E9F6E', successBg: '#E2F2EA', warning: '#B5710C', warningBg: '#FBEFD8', danger: '#C2410C', dangerBg: '#FBE6DC', ring: '#0E9F6E' },
    dark: { bg: '#1A1815', surface: '#232019', surface2: '#2E2A22', border: '#3D382E', text: '#F2EDE3', textMuted: '#A89E8D', accent: '#34C893', accentFg: '#11241B', accentHover: '#48D6A3', accentSoft: '#1F3329', success: '#34C893', successBg: '#1F3329', warning: '#E0A23C', warningBg: '#352B19', danger: '#F0794A', dangerBg: '#3A241A', ring: '#34C893' },
  },
}
