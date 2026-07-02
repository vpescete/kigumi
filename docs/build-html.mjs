#!/usr/bin/env node
// Pre-renders the Markdown docs (docs/guida/<lang>/*.md) into static, themed HTML pages under
// landing/docs/<lang>/<slug>.html — no runtime fetch, openable straight from disk. Re-run after
// editing the Markdown:  node docs/build-html.mjs
//
// Requires pandoc on PATH. The Markdown stays the single source of truth; this only projects it.

import { execFileSync } from 'node:child_process'
import { readdirSync, mkdirSync, writeFileSync, existsSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..')
const SRC = join(ROOT, 'docs', 'guida')
const OUT = join(ROOT, 'landing', 'docs')
const GH = 'https://github.com/vpescete/msh_framework/blob/main/docs'

// Page order + per-language sidebar labels. Slug = filename without .md.
const ORDER = ['README', 'architettura', 'installazione', 'configurazione', 'moduli', 'moduli-custom', 'api', 'sicurezza']
const LABEL = {
  it: { README: 'Panoramica', architettura: 'Architettura', installazione: 'Installazione', configurazione: 'Configurazione', moduli: 'Moduli', 'moduli-custom': 'Moduli custom', api: 'API e contratto-UI', sicurezza: 'Sicurezza' },
  en: { README: 'Overview', architettura: 'Architecture', installazione: 'Installation', configurazione: 'Configuration', moduli: 'Modules', 'moduli-custom': 'Custom modules', api: 'API & UI contract', sicurezza: 'Security' },
}
const UI = {
  it: { docs: 'Documentazione', back: '← Landing', other: 'EN', land: '../../it/index.html' },
  en: { docs: 'Documentation', back: '← Landing', other: 'IT', land: '../../index.html' },
}
const OTHER = { it: 'en', en: 'it' }

const HEAD = `  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <link rel="preconnect" href="https://fonts.googleapis.com" />
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
  <link href="https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@400;500;600;700&family=Hanken+Grotesk:wght@400;500;600&family=JetBrains+Mono:wght@400;500;600&display=swap" rel="stylesheet" />
  <script src="https://cdn.tailwindcss.com"></script>
  <script>
    tailwind.config = { theme: { extend: { colors: {
      bg: '#0D1014', surface: '#14181D', surface2: '#1B2026', border: '#272D34',
      text: '#E5E9ED', muted: '#8B949E', accent: '#22B8CF', accentfg: '#06181C',
      accenthover: '#46C8DB', success: '#3DD68C', violet: '#7B8CFF',
    }, fontFamily: {
      display: ['Space Grotesk', 'ui-sans-serif', 'system-ui', 'sans-serif'],
      body: ['Hanken Grotesk', 'ui-sans-serif', 'system-ui', 'sans-serif'],
      mono: ['JetBrains Mono', 'ui-monospace', 'monospace'],
    } } } }
  </script>
  <style>
    :root { color-scheme: dark; }
    body { background:#0d1014; color:#e5e9ed; font-family:'Hanken Grotesk',ui-sans-serif,system-ui,sans-serif; -webkit-font-smoothing:antialiased; text-rendering:optimizeLegibility; }
    .scanline { position:relative; }
    .scanline::before { content:''; position:absolute; left:0; top:.15em; bottom:.15em; width:2px; border-radius:999px; background:#22b8cf; }
    .pulse { animation:pulse 2.4s ease-in-out infinite; }
    @keyframes pulse { 0%,100%{opacity:1} 50%{opacity:.35} }
    @media (prefers-reduced-motion: reduce){ .pulse{animation:none} }
    .markdown { font-size:15.5px; line-height:1.7; color:#c8cfd6; }
    .markdown > :first-child { margin-top:0; }
    .markdown h1,.markdown h2,.markdown h3,.markdown h4 { font-family:'Space Grotesk',ui-sans-serif,system-ui,sans-serif; color:#e5e9ed; font-weight:600; letter-spacing:-.01em; line-height:1.25; }
    .markdown h1 { font-size:2rem; margin:0 0 .75rem; letter-spacing:-.02em; }
    .markdown h2 { font-size:1.5rem; margin:2.6rem 0 1rem; padding-top:1.4rem; border-top:1px solid #272d34; }
    .markdown h3 { font-size:1.18rem; margin:2rem 0 .7rem; }
    .markdown h4 { font-size:1rem; margin:1.6rem 0 .5rem; color:#cdd4db; }
    .markdown p { margin:0 0 1.05rem; }
    .markdown a { color:#46c8db; text-decoration:none; border-bottom:1px solid rgba(70,200,219,.3); }
    .markdown a:hover { color:#22b8cf; border-bottom-color:#22b8cf; }
    .markdown strong { color:#e5e9ed; font-weight:600; }
    .markdown ul,.markdown ol { margin:0 0 1.05rem; padding-left:1.35rem; }
    .markdown li { margin:.3rem 0; }
    .markdown li::marker { color:#5b636c; }
    .markdown code { font-family:'JetBrains Mono',ui-monospace,monospace; font-size:.86em; background:#1b2026; border:1px solid #272d34; border-radius:5px; padding:.1em .38em; color:#cdd4db; }
    .markdown pre { background:#0f1216; border:1px solid #272d34; border-radius:8px; padding:1rem 1.1rem; overflow-x:auto; margin:0 0 1.2rem; }
    .markdown pre code { font-size:12.8px; line-height:1.65; background:none; border:0; padding:0; color:#d7dde3; }
    .markdown blockquote { margin:0 0 1.2rem; padding:.2rem 0 .2rem 1rem; border-left:2px solid #22b8cf; color:#9aa3ac; }
    .markdown hr { border:0; border-top:1px solid #272d34; margin:2rem 0; }
    .markdown table { width:100%; border-collapse:collapse; margin:0 0 1.3rem; font-size:14px; display:block; overflow-x:auto; }
    .markdown th,.markdown td { border:1px solid #272d34; padding:.5rem .7rem; text-align:left; }
    .markdown th { background:#1b2026; color:#e5e9ed; font-weight:600; }
    .markdown td code { white-space:nowrap; }
    a,button { transition:color .16s ease,background-color .16s ease,border-color .16s ease; }
    .docnav::-webkit-scrollbar { height:6px; width:6px; }
    .docnav::-webkit-scrollbar-thumb { background:#272d34; border-radius:999px; }
  </style>`

// Rewrite Markdown links in the rendered HTML: sibling .md -> .html, external ../*.md -> GitHub.
function fixLinks(html) {
  return html
    .replace(/href="(?:\.\.\/)+([^"]+?\.md)(#[^"]*)?"/g, (m, file, frag) => `href="${GH}/${file}${frag || ''}"`)
    .replace(/href="(?:\.\/)?([^"/:]+?)\.md(#[^"]*)?"/g, (m, slug, frag) => `href="${slug}.html${frag || ''}"`)
}

function sidebar(lang, current) {
  return ORDER.map((slug) => {
    const on = slug === current
    const cls = on
      ? 'scanline shrink-0 whitespace-nowrap rounded-md bg-surface px-3 py-1.5 text-[14px] text-text lg:whitespace-normal'
      : 'shrink-0 whitespace-nowrap rounded-md px-3 py-1.5 text-[14px] text-muted hover:bg-surface hover:text-text lg:whitespace-normal'
    return `        <a href="${slug}.html" class="${cls}">${LABEL[lang][slug]}</a>`
  }).join('\n')
}

function page(lang, slug, bodyHtml) {
  const ui = UI[lang]
  const title = LABEL[lang][slug]
  return `<!doctype html>
<html lang="${lang}" class="scroll-smooth">
<head>
  <title>Kigumi · ${title} · ${lang.toUpperCase()}</title>
${HEAD}
</head>
<body class="font-body antialiased">
  <header class="sticky top-0 z-40 border-b border-border/80 bg-bg/85 backdrop-blur">
    <div class="mx-auto flex h-16 max-w-[1180px] items-center justify-between gap-6 px-5 sm:px-8">
      <a href="${ui.land}" class="flex items-center gap-2">
        <span class="pulse h-2 w-2 rounded-full bg-accent"></span>
        <span class="font-display text-[17px] font-semibold text-text">kigumi</span>
        <span class="font-mono text-[12px] text-muted">/ docs</span>
      </a>
      <div class="flex items-center gap-5 text-[14px] text-muted">
        <a class="hover:text-text" href="${ui.land}">${ui.back}</a>
        <div class="flex items-center gap-1 font-mono text-[12px]">
          <span class="rounded bg-surface px-1.5 py-0.5 text-text">${lang.toUpperCase()}</span>
          <a class="rounded px-1.5 py-0.5 hover:text-text" href="../${OTHER[lang]}/${slug}.html">${ui.other}</a>
        </div>
        <a class="hover:text-text" href="https://github.com/vpescete/msh_framework" target="_blank" rel="noopener">GitHub</a>
      </div>
    </div>
  </header>
  <div class="mx-auto max-w-[1180px] px-5 sm:px-8 lg:grid lg:grid-cols-[232px_minmax(0,1fr)] lg:gap-10">
    <aside class="lg:sticky lg:top-16 lg:h-[calc(100vh-4rem)] lg:overflow-y-auto py-6 lg:py-9">
      <div class="mb-3 hidden font-mono text-[11px] uppercase tracking-[0.16em] text-muted lg:block">${ui.docs}</div>
      <nav class="docnav flex gap-2 overflow-x-auto pb-2 lg:flex-col lg:gap-0.5 lg:overflow-visible lg:pb-0">
${sidebar(lang, slug)}
      </nav>
    </aside>
    <main class="min-w-0 py-8 lg:py-9">
      <article class="markdown max-w-[760px]">
${bodyHtml}
      </article>
      <footer class="mt-16 border-t border-border pt-6 text-[13px] text-muted">
        <span class="font-mono">kigumi · ${ui.docs.toLowerCase()}</span>
      </footer>
    </main>
  </div>
</body>
</html>
`
}

function redirectIndex() {
  // Bare /docs/ -> English overview (the landing default). Each landing language links straight to its lang dir.
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="refresh" content="0; url=./en/README.html" />
  <title>Kigumi · Documentation</title>
</head>
<body><a href="./en/README.html">Kigumi documentation</a></body>
</html>
`
}

let total = 0
for (const lang of ['it', 'en']) {
  const dir = join(SRC, lang)
  if (!existsSync(dir)) continue
  const files = readdirSync(dir).filter((f) => f.endsWith('.md'))
  if (!files.length) continue
  const outDir = join(OUT, lang)
  mkdirSync(outDir, { recursive: true })
  for (const f of files) {
    const slug = f.replace(/\.md$/, '')
    const body = execFileSync('pandoc', [join(dir, f), '-f', 'gfm', '-t', 'html', '--wrap=none'], { encoding: 'utf8' })
    writeFileSync(join(outDir, `${slug}.html`), page(lang, slug, fixLinks(body)))
    total++
  }
  console.log(`  ${lang}: ${files.length} pages`)
}
mkdirSync(OUT, { recursive: true })
writeFileSync(join(OUT, 'index.html'), redirectIndex())
console.log(`Generated ${total} HTML pages + index in landing/docs/`)
