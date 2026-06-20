import { useEffect, useMemo, useState } from 'react'
import { Check, Copy, Download, Save, Trash2, Wand2 } from 'lucide-react'
import {
  COLOR_TOKENS,
  TYPE_ROLES,
  type ColorToken,
  type Mode,
  type Theme,
} from '../theme/contract'
import { injectOne, themeToCss } from '../theme/css'
import { isCustom, removeCustomTheme, upsertCustomTheme } from '../theme/registry'
import { hexToRgb, lintTheme, pairRatios } from '../theme/validate'
import { useTheme } from '../theme'
import { Button, Card, PageHeader, StateBadge } from '../ui'

const kebab = (s: string) =>
  s.toLowerCase().trim().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '') || 'my-theme'

// <input type="color"> needs exactly #rrggbb — normalise 3/8-digit hex (and reject junk) to that.
const toPickerHex = (v: string): string => {
  const rgb = hexToRgb(v)
  return rgb ? '#' + rgb.map((c) => c.toString(16).padStart(2, '0')).join('') : '#000000'
}

const PREVIEW_ID = '__draft'

export function ThemeStudio() {
  const { themes, theme: activeId, setTheme } = useTheme()
  const base = themes.find((t) => t.id === activeId) ?? themes[0]

  const [draft, setDraft] = useState<Theme>(() => ({
    ...structuredClone(base),
    id: 'my-theme',
    name: 'My Theme',
    author: 'you',
    version: '0.1.0',
  }))
  const [mode, setMode] = useState<Mode>(draft.defaultMode)
  const [copied, setCopied] = useState(false)
  const [saved, setSaved] = useState(false)

  // Live preview: re-inject the draft under a fixed id so the scoped preview pane updates as you edit.
  useEffect(() => {
    injectOne({ ...draft, id: PREVIEW_ID })
  }, [draft])

  const lints = useMemo(() => lintTheme(draft), [draft])
  const errors = lints.filter((l) => l.level === 'error')
  const ratios = pairRatios(draft.color[mode])

  const setName = (name: string) => setDraft((d) => ({ ...d, name, id: kebab(name) }))
  const setFont = (k: 'display' | 'body' | 'mono', v: string) =>
    setDraft((d) => ({ ...d, fonts: { ...d.fonts, [k]: v } }))
  const setColor = (tok: ColorToken, v: string) =>
    setDraft((d) => ({ ...d, color: { ...d.color, [mode]: { ...d.color[mode], [tok]: v } } }))
  const setRadius = (k: 'sm' | 'md' | 'lg', v: string) =>
    setDraft((d) => ({ ...d, radius: { ...d.radius, [k]: v } }))
  const setDensity = (k: 'row' | 'control' | 'fsBase', v: string) =>
    setDraft((d) => ({ ...d, density: { ...d.density, [k]: v } }))
  const setRoleSize = (role: (typeof TYPE_ROLES)[number], v: string) =>
    setDraft((d) => ({ ...d, type: { ...d.type, [role]: { ...d.type[role], size: v } } }))
  const forkFrom = (id: string) => {
    const src = themes.find((t) => t.id === id)
    if (!src) return
    setDraft((d) => ({ ...structuredClone(src), id: d.id, name: d.name, author: d.author, version: d.version }))
  }

  const save = () => {
    const res = upsertCustomTheme(draft)
    if (res.ok) {
      setSaved(true)
      setTimeout(() => setSaved(false), 1500)
    }
  }
  const apply = () => {
    if (upsertCustomTheme(draft).ok) setTheme(draft.id)
  }
  const exportJson = () => {
    const blob = new Blob([JSON.stringify(draft, null, 2)], { type: 'application/json' })
    const a = document.createElement('a')
    a.href = URL.createObjectURL(blob)
    a.download = `${draft.id}.theme.json`
    a.click()
    URL.revokeObjectURL(a.href)
  }
  const copyCss = async () => {
    await navigator.clipboard.writeText(themeToCss(draft))
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }

  return (
    <div>
      <PageHeader
        title="Theme Studio"
        subtitle="Fork a base, tune the tokens, watch it live. Save it and it joins the switcher; export the JSON to ship it."
        actions={
          <>
            <Button variant="ghost" icon={copied ? <Check size={16} /> : <Copy size={16} />} onClick={copyCss}>
              {copied ? 'Copied' : 'Copy CSS'}
            </Button>
            <Button variant="secondary" icon={<Download size={16} />} onClick={exportJson}>Export JSON</Button>
            <Button variant="secondary" icon={saved ? <Check size={16} /> : <Save size={16} />} onClick={save}>
              {saved ? 'Saved' : 'Save'}
            </Button>
            <Button variant="primary" icon={<Wand2 size={16} />} onClick={apply}>Apply to app</Button>
          </>
        }
      />

      <div className="grid grid-cols-1 lg:grid-cols-[minmax(0,380px)_1fr] gap-5 items-start">
        {/* ── Controls ── */}
        <div className="space-y-4">
          <Card className="p-4 space-y-3">
            <Section>Identity</Section>
            <Labeled label="Name">
              <TextInput value={draft.name} onChange={setName} />
            </Labeled>
            <div className="grid grid-cols-2 gap-2">
              <Labeled label="id">
                <code className="t-mono text-muted">{draft.id}</code>
              </Labeled>
              <Labeled label="Fork from">
                <Select value="" onChange={forkFrom} options={themes.map((t) => ({ value: t.id, label: t.name }))} placeholder="choose base…" />
              </Labeled>
            </div>
            <Labeled label="Default mode">
              <div className="flex gap-1">
                {(['light', 'dark'] as Mode[]).map((m) => (
                  <button
                    key={m}
                    onClick={() => setDraft((d) => ({ ...d, defaultMode: m }))}
                    className={
                      't-label px-3 rounded-md border border-border ' +
                      (draft.defaultMode === m ? 'bg-accent text-accent-fg' : 'text-muted')
                    }
                    style={{ height: 'var(--control-h)' }}
                  >
                    {m}
                  </button>
                ))}
              </div>
            </Labeled>
          </Card>

          <Card className="p-4 space-y-3">
            <Section>Typography</Section>
            <Labeled label="Display font"><TextInput value={draft.fonts.display} onChange={(v) => setFont('display', v)} mono /></Labeled>
            <Labeled label="Body font"><TextInput value={draft.fonts.body} onChange={(v) => setFont('body', v)} mono /></Labeled>
            <Labeled label="Mono font"><TextInput value={draft.fonts.mono} onChange={(v) => setFont('mono', v)} mono /></Labeled>
            <div className="grid grid-cols-4 gap-2 pt-1">
              {TYPE_ROLES.map((r) => (
                <Labeled key={r} label={r}>
                  <TextInput value={draft.type[r].size} onChange={(v) => setRoleSize(r, v)} mono />
                </Labeled>
              ))}
            </div>
          </Card>

          <Card className="p-4 space-y-3">
            <Section>Shape &amp; density</Section>
            <div className="grid grid-cols-3 gap-2">
              <Labeled label="radius sm"><TextInput value={draft.radius.sm} onChange={(v) => setRadius('sm', v)} mono /></Labeled>
              <Labeled label="radius md"><TextInput value={draft.radius.md} onChange={(v) => setRadius('md', v)} mono /></Labeled>
              <Labeled label="radius lg"><TextInput value={draft.radius.lg} onChange={(v) => setRadius('lg', v)} mono /></Labeled>
              <Labeled label="row h"><TextInput value={draft.density.row} onChange={(v) => setDensity('row', v)} mono /></Labeled>
              <Labeled label="control h"><TextInput value={draft.density.control} onChange={(v) => setDensity('control', v)} mono /></Labeled>
              <Labeled label="base font"><TextInput value={draft.density.fsBase} onChange={(v) => setDensity('fsBase', v)} mono /></Labeled>
            </div>
          </Card>

          <Card className="p-4 space-y-3">
            <div className="flex items-center justify-between">
              <Section>Colors</Section>
              <div className="flex gap-1">
                {(['light', 'dark'] as Mode[]).map((m) => (
                  <button
                    key={m}
                    onClick={() => setMode(m)}
                    className={'t-label px-2.5 py-1 rounded-md ' + (mode === m ? 'bg-accent text-accent-fg' : 'text-muted hover:text-text')}
                  >
                    {m}
                  </button>
                ))}
              </div>
            </div>
            <div className="grid grid-cols-2 gap-x-3 gap-y-2">
              {COLOR_TOKENS.map((tok) => (
                <ColorRow key={tok} token={tok} value={draft.color[mode][tok]} onChange={(v) => setColor(tok, v)} />
              ))}
            </div>
          </Card>

          <Card className="p-4 space-y-2">
            <Section>Contrast ({mode})</Section>
            {ratios.map((r) => (
              <div key={r.label} className="flex items-center justify-between t-caption">
                <span className="text-muted">{r.label}</span>
                <span className={r.pass ? 'text-success' : 'text-danger'}>
                  {r.ratio.toFixed(2)}:1 {r.pass ? '✓' : '✗ (< 4.5)'}
                </span>
              </div>
            ))}
            {errors.length > 0 && (
              <div className="t-caption text-danger pt-1">{errors.map((e) => e.message).join(' · ')}</div>
            )}
            {isCustom(draft.id) && (
              <Button variant="ghost" icon={<Trash2 size={15} />} onClick={() => removeCustomTheme(draft.id)} className="mt-1 !text-danger">
                Delete saved theme
              </Button>
            )}
          </Card>
        </div>

        {/* ── Live preview (scoped to the draft) ── */}
        <div data-theme={PREVIEW_ID} data-mode={mode} className="bg-bg text-text rounded-lg border border-border overflow-hidden sticky top-0">
          <PreviewBoard />
        </div>
      </div>
    </div>
  )
}

/* ── Preview composition: the design system applied to real components ── */
function PreviewBoard() {
  return (
    <div className="p-6 space-y-6" style={{ fontFamily: 'var(--font-body)' }}>
      <div>
        <div className="t-display text-text">Aa Meshble</div>
        <div className="t-h1 text-text mt-2">Sales Orders</div>
        <div className="t-h2 text-text mt-1.5">Order S00012</div>
        <div className="t-subtitle text-muted mt-1.5">Acme Corporation · 09 Jun 2026</div>
        <div className="t-body text-text mt-2">Body text — the quick brown fox jumps over the lazy dog 0123456789.</div>
        <div className="t-label text-muted mt-2">Overline label</div>
        <div className="t-mono text-text mt-1">S00012 · €4,140.00 · qty 25</div>
      </div>

      <div className="flex flex-wrap gap-2 items-center">
        <Button variant="primary">Confirm</Button>
        <Button variant="secondary">Cancel</Button>
        <Button variant="ghost">Print</Button>
        <input
          placeholder="Search…"
          className="t-body bg-surface2 border border-border rounded-md px-3"
          style={{ height: 'var(--control-h)' }}
        />
      </div>

      <div className="flex flex-wrap gap-2">
        <StateBadge state="draft" />
        <StateBadge state="sent" />
        <StateBadge state="done" />
        <StateBadge state="cancel" />
      </div>

      <Card className="overflow-hidden">
        <div className="px-4 py-3 border-b border-border flex justify-between items-center">
          <span className="t-h2 text-text">Order lines</span>
          <span className="t-mono text-accent">€4,140.00</span>
        </div>
        <table className="w-full border-collapse">
          <thead>
            <tr className="border-b border-border">
              <th className="t-label text-muted text-left px-4 py-2">Product</th>
              <th className="t-label text-muted text-right px-4 py-2 w-16">Qty</th>
              <th className="t-label text-muted text-right px-4 py-2 w-28">Subtotal</th>
            </tr>
          </thead>
          <tbody>
            {[
              ['License — Pro (seat/yr)', 4, '€960.00'],
              ['Onboarding package', 1, '€1,200.00'],
              ['Support — Premium', 1, '€600.00'],
            ].map(([p, q, s]) => (
              <tr key={p as string} className="border-b border-border last:border-0 hover:bg-surface2" style={{ height: 'var(--density-row)' }}>
                <td className="px-4 t-body text-text">{p}</td>
                <td className="px-4 text-right t-mono text-muted">{q}</td>
                <td className="px-4 text-right t-mono text-text">{s}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Card>

      <div className="grid grid-cols-9 gap-1.5">
        {COLOR_TOKENS.map((tok) => (
          <div key={tok} className="h-7 rounded-sm border border-border" style={{ background: `var(--color-${tok.replace(/[A-Z]/g, (m) => '-' + m.toLowerCase())})` }} title={tok} />
        ))}
      </div>
    </div>
  )
}

/* ── Small control primitives (rendered in the APP theme, not the draft) ── */
function Section({ children }: { children: React.ReactNode }) {
  return <div className="t-label text-muted">{children}</div>
}
function Labeled({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <div className="t-caption text-muted mb-1">{label}</div>
      {children}
    </label>
  )
}
function TextInput({ value, onChange, mono }: { value: string; onChange: (v: string) => void; mono?: boolean }) {
  return (
    <input
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className={(mono ? 't-mono ' : 't-body ') + 'w-full bg-input border border-input-border rounded-md px-2 text-text shadow-xs transition-[box-shadow,border-color] duration-fast ease-out hover:border-muted focus:outline-none focus-visible:border-accent focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:shadow-focus'}
      style={{ height: 'var(--control-h)' }}
    />
  )
}
function Select({ value, onChange, options, placeholder }: { value: string; onChange: (v: string) => void; options: { value: string; label: string }[]; placeholder?: string }) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className="t-body w-full bg-surface2 border border-border rounded-md px-2 text-text"
      style={{ height: 'var(--control-h)' }}
    >
      {placeholder && <option value="">{placeholder}</option>}
      {options.map((o) => (
        <option key={o.value} value={o.value}>{o.label}</option>
      ))}
    </select>
  )
}
function ColorRow({ token, value, onChange }: { token: string; value: string; onChange: (v: string) => void }) {
  return (
    <div className="flex items-center gap-2">
      <input type="color" value={toPickerHex(value)} onChange={(e) => onChange(e.target.value)} className="h-7 w-7 rounded border border-border bg-transparent shrink-0" />
      <div className="min-w-0 flex-1">
        <div className="t-caption text-muted truncate">{token}</div>
        <input value={value} onChange={(e) => onChange(e.target.value)} className="t-mono w-full bg-transparent text-text focus:outline-none" />
      </div>
    </div>
  )
}
