import { useCallback, useEffect, useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { Activity, ArrowUpRight, MessageSquare, Plus, RefreshCw } from 'lucide-react'
import * as api from '../api'
import { cx, focusRing, Sparkline, SkeletonStat, SkeletonTable } from '../ui'
import { modelTitle } from '../format'

// KPI models with an optional real secondary metric (a domain-filtered count). Sparklines come from
// real message activity (mail.message grouped by res_model) — no fabricated trends.
const KPIS: { model: string; label: string; secondary?: { label: string; domain: unknown } }[] = [
  { model: 'sale.order', label: 'Sales orders', secondary: { label: 'to invoice', domain: { field: 'invoice_status', op: '=', value: 'to_invoice' } } },
  { model: 'account.move', label: 'Journal entries', secondary: { label: 'draft', domain: { field: 'state', op: '=', value: 'draft' } } },
  { model: 'res.partner', label: 'Partners' },
  { model: 'product.product', label: 'Products' },
]
const QUICK = [
  { model: 'sale.order', label: 'New sales order' },
  { model: 'res.partner', label: 'New partner' },
  { model: 'product.product', label: 'New product' },
]

interface Kpi {
  model: string
  label: string
  total: number
  secondary?: { label: string; value: number }
  series: number[]
}

function dayKey(d: Date): string {
  return d.toISOString().slice(0, 10)
}

/** A real per-day activity series for `model` from chatter messages (count of messages per day). */
function seriesFor(messages: api.Row[], model: string, days = 14): number[] {
  const counts = new Map<string, number>()
  for (const m of messages) {
    if (m.res_model !== model) continue
    const d = new Date(String(m.date ?? '').replace(' ', 'T'))
    if (Number.isNaN(d.getTime())) continue
    const k = dayKey(d)
    counts.set(k, (counts.get(k) ?? 0) + 1)
  }
  const now = new Date()
  return Array.from({ length: days }, (_, i) => {
    const d = new Date(now)
    d.setDate(now.getDate() - (days - 1 - i))
    return counts.get(dayKey(d)) ?? 0
  })
}

function relTime(iso: string | null): string {
  if (!iso) return ''
  const d = new Date(iso.replace(' ', 'T'))
  const s = Math.max(0, (Date.now() - d.getTime()) / 1000)
  if (s < 60) return `${Math.floor(s)}s`
  if (s < 3600) return `${Math.floor(s / 60)}m`
  if (s < 86400) return `${Math.floor(s / 3600)}h`
  return `${Math.floor(s / 86400)}d`
}

export function Dashboard() {
  const nav = useNavigate()
  const [kpis, setKpis] = useState<Kpi[] | null>(null)
  const [messages, setMessages] = useState<api.Row[]>([])
  const [syncedAt, setSyncedAt] = useState<Date | null>(null)
  const [loading, setLoading] = useState(true)

  const load = useCallback(async (): Promise<void> => {
    setLoading(true)
    // Chatter activity feeds both the sparklines and the recent-activity stream (best-effort).
    const msgs = await api.list('mail.message', { limit: 200, order: '-id' }).then((p) => p.data).catch(() => [] as api.Row[])
    const built = await Promise.all(
      KPIS.map(async (k) => {
        const total = await api.list(k.model, { limit: 1 }).then((p) => p.total).catch(() => 0)
        let secondary: Kpi['secondary']
        if (k.secondary) {
          const value = await api.list(k.model, { limit: 1, domain: k.secondary.domain }).then((p) => p.total).catch(() => 0)
          secondary = { label: k.secondary.label, value }
        }
        return { model: k.model, label: k.label, total, secondary, series: seriesFor(msgs, k.model) }
      }),
    )
    setMessages(msgs)
    setKpis(built)
    setSyncedAt(new Date())
    setLoading(false)
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  const empty = useMemo(() => kpis != null && kpis.every((k) => k.total === 0), [kpis])

  return (
    <div>
      {/* Live strip — the one terminal-style flourish. */}
      <div className="mb-5 flex items-center justify-between border-b border-border pb-3">
        <div className="flex items-center gap-2">
          <span className="msh-pulse h-1.5 w-1.5 rounded-full bg-success" aria-hidden="true" />
          <span className="t-mono text-[11px] text-muted">meshble · live · contract-driven</span>
        </div>
        <div className="flex items-center gap-3">
          {syncedAt && <span className="t-mono text-[11px] text-muted">synced {syncedAt.toLocaleTimeString()}</span>}
          <button onClick={() => void load()} aria-label="Refresh" className={cx('rounded-md p-1 text-muted hover:text-text', focusRing)}>
            <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
          </button>
        </div>
      </div>

      <div className="mb-6 flex items-end justify-between gap-4">
        <div>
          <h1 className="t-h1 text-text">Dashboard</h1>
          <p className="t-subtitle text-muted mt-1">Live operational overview</p>
        </div>
        <button
          onClick={() => nav('/m/sale.order/new')}
          className={cx('inline-flex items-center gap-2 rounded-md bg-accent px-3 font-medium text-accent-fg hover:bg-accent-hover', focusRing)}
          style={{ height: 'var(--control-h)' }}
        >
          <Plus size={16} /> New sales order
        </button>
      </div>

      {loading && !kpis ? (
        <div className="mb-7 grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
          {KPIS.map((k) => (
            <SkeletonStat key={k.model} />
          ))}
        </div>
      ) : empty ? (
        <EmptyState onCreate={() => nav('/m/sale.order/new')} />
      ) : (
        <>
          <div className="mb-7 grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
            {kpis?.map((k) => <KpiTile key={k.model} kpi={k} onOpen={() => nav(`/m/${k.model}`)} />)}
          </div>

          <div className="grid grid-cols-1 gap-5 lg:grid-cols-3">
            <div className="lg:col-span-2">
              <ActivityStream messages={messages} loading={loading} onOpen={(m, id) => nav(`/m/${m}/${id}`)} />
            </div>
            <QuickActions onPick={(m) => nav(`/m/${m}/new`)} />
          </div>
        </>
      )}
    </div>
  )
}

function KpiTile({ kpi, onOpen }: { kpi: Kpi; onOpen: () => void }) {
  return (
    <button
      onClick={onOpen}
      className={cx(
        'group relative overflow-hidden rounded-lg border border-border bg-surface p-4 text-left',
        'hover:border-accent/40',
        focusRing,
      )}
    >
      {/* the cyan scanline appears on hover/focus */}
      <span className="absolute inset-y-0 left-0 w-0.5 bg-accent opacity-0 transition-opacity duration-base group-hover:opacity-100 group-focus-visible:opacity-100" aria-hidden="true" />
      <div className="flex items-center justify-between">
        <span className="t-label text-muted">{kpi.label}</span>
        <ArrowUpRight size={14} className="text-muted opacity-0 transition-opacity group-hover:opacity-100" />
      </div>
      <div className="mt-2 font-mono text-[28px] font-semibold leading-none tabular-nums text-text">{kpi.total.toLocaleString()}</div>
      <div className="mt-1.5 flex h-7 items-end justify-between gap-2">
        <span className="t-caption text-muted">
          {kpi.secondary ? (
            <>
              <span className="font-mono tabular-nums text-text">{kpi.secondary.value}</span> {kpi.secondary.label}
            </>
          ) : (
            'records'
          )}
        </span>
        <Sparkline values={kpi.series} fill="var(--viz-1)" width={84} height={26} />
      </div>
    </button>
  )
}

function ActivityStream({ messages, loading, onOpen }: { messages: api.Row[]; loading: boolean; onOpen: (model: string, id: number) => void }) {
  if (loading && messages.length === 0) return <SkeletonTable rows={5} cols={3} />
  return (
    <div className="rounded-lg border border-border bg-surface">
      <div className="t-label flex items-center gap-2 border-b border-border px-4 py-2.5 text-muted">
        <Activity size={13} /> Recent activity
      </div>
      {messages.length === 0 ? (
        <div className="t-body px-4 py-8 text-center text-muted">No activity yet.</div>
      ) : (
        <ul>
          {messages.slice(0, 10).map((m) => {
            const model = String(m.res_model ?? '')
            const id = Number(m.res_id)
            return (
              <li key={m.id as number}>
                <button
                  onClick={() => onOpen(model, id)}
                  className={cx('flex w-full items-center gap-3 border-b border-border px-4 py-2.5 text-left last:border-0 hover:bg-surface2', focusRing)}
                >
                  <MessageSquare size={14} className="shrink-0 text-muted" />
                  <span className="t-body min-w-0 flex-1 truncate text-text">
                    {modelTitle(model)} <span className="t-mono text-muted">#{id}</span>
                    <span className="t-caption ml-1.5 text-muted">· {String(m.message_type ?? '')}</span>
                  </span>
                  <span className="t-mono shrink-0 text-[11px] text-muted">{relTime(m.date as string | null)}</span>
                </button>
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}

function QuickActions({ onPick }: { onPick: (model: string) => void }) {
  return (
    <div className="rounded-lg border border-border bg-surface">
      <div className="t-label border-b border-border px-4 py-2.5 text-muted">Quick actions</div>
      <div className="p-2">
        {QUICK.map((q) => (
          <button
            key={q.model}
            onClick={() => onPick(q.model)}
            className={cx('flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-muted hover:bg-surface2 hover:text-text', focusRing)}
          >
            <Plus size={15} className="text-accent" />
            <span className="t-body">{q.label}</span>
          </button>
        ))}
      </div>
    </div>
  )
}

function EmptyState({ onCreate }: { onCreate: () => void }) {
  return (
    <div className="rounded-lg border border-dashed border-border bg-surface px-6 py-16 text-center">
      <div className="t-h2 text-text">Nothing's moving yet</div>
      <p className="t-body mx-auto mt-2 max-w-sm text-muted">Create your first sales order to see the pulse — orders, invoices and activity all show up here.</p>
      <button
        onClick={onCreate}
        className={cx('mt-5 inline-flex items-center gap-2 rounded-md bg-accent px-3.5 font-medium text-accent-fg hover:bg-accent-hover', focusRing)}
        style={{ height: 'var(--control-h)' }}
      >
        <Plus size={16} /> New sales order
      </button>
    </div>
  )
}
