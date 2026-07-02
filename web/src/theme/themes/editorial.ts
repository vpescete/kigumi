import type { Theme } from '../contract'

// Editorial — A sophisticated magazine-grade light interface for operators who read their ERP like a well-set page — warm, calm, and confident.
export const editorial: Theme = {
  id: 'editorial',
  name: 'Editorial',
  author: 'Kigumi',
  version: '1.0.0',
  compat: '^0.1',
  defaultMode: 'light',
  fontImports: ['https://fonts.googleapis.com/css2?family=Fraunces:opsz,wght@9..144,400;9..144,500;9..144,600;9..144,700&family=Hanken+Grotesk:wght@400;500;600;700&family=IBM+Plex+Mono:wght@400;500;600&display=swap'],
  fonts: { display: "'Fraunces', Georgia, 'Times New Roman', serif", body: "'Hanken Grotesk', ui-sans-serif, system-ui, -apple-system, sans-serif", mono: "'IBM Plex Mono', ui-monospace, SFMono-Regular, Menlo, monospace" },
  type: {
    display: { stack: 'display', size: '52px', weight: 600, lh: '1.04', tracking: '-0.025em', transform: 'none' },
    h1: { stack: 'display', size: '34px', weight: 600, lh: '1.1', tracking: '-0.02em', transform: 'none' },
    h2: { stack: 'body', size: '22px', weight: 600, lh: '1.2', tracking: '-0.01em', transform: 'none' },
    subtitle: { stack: 'body', size: '17px', weight: 500, lh: '1.4', tracking: '0', transform: 'none' },
    body: { stack: 'body', size: '15px', weight: 400, lh: '1.55', tracking: '0', transform: 'none' },
    label: { stack: 'body', size: '11px', weight: 600, lh: '1.3', tracking: '0.08em', transform: 'uppercase' },
    caption: { stack: 'body', size: '12.5px', weight: 400, lh: '1.45', tracking: '0.01em', transform: 'none' },
    mono: { stack: 'mono', size: '13px', weight: 400, lh: '1.45', tracking: '0', transform: 'none' }
  },
  radius: { sm: '5px', md: '9px', lg: '14px' },
  shadow: { sm: "0 1px 2px rgba(78, 56, 44, 0.06), 0 1px 1px rgba(78, 56, 44, 0.04)", md: "0 4px 14px rgba(78, 56, 44, 0.09), 0 2px 6px rgba(78, 56, 44, 0.06)" },
  density: { row: '50px', control: '38px', fsBase: '15px', space: '8px' },
  color: {
    light: { bg: '#F7F3EC', surface: '#FFFDF8', surface2: '#F1EBE0', border: '#E0D7C8', text: '#2E2823', textMuted: '#766B5E', accent: '#B75A3A', accentFg: '#FFFFFF', accentHover: '#A94F30', accentSoft: '#F3E2D7', success: '#4F7A45', successBg: '#E6EEDD', warning: '#B07A1E', warningBg: '#F5E9CF', danger: '#B23A2E', dangerBg: '#F6DDD7', ring: '#C2613F' },
    dark: { bg: '#211D18', surface: '#2A251F', surface2: '#332D26', border: '#433B31', text: '#F0E9DD', textMuted: '#AFA399', accent: '#D87B57', accentFg: '#241007', accentHover: '#E69270', accentSoft: '#3D2C22', success: '#8FB97E', successBg: '#2C3724', warning: '#D6A24E', warningBg: '#3A301B', danger: '#E27A6C', dangerBg: '#3C241F', ring: '#D87B57' },
  },
}
