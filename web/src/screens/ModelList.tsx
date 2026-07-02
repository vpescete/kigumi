import { useEffect, useState } from 'react'
import { useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { Filter, Inbox, Plus, X } from 'lucide-react'
import * as api from '../api'
import type { Column } from '../ui'
import { Button, cx, DataTable, ErrorState, focusRing, PageHeader, SkeletonTable } from '../ui'
import { buildResolver, displayValue, modelTitle, relLabel, type Resolver } from '../format'
import { useEventStream } from '../useEventStream'

const noResolve: Resolver = () => undefined

// A list view rendered entirely from the model's contract (columns from list.columns), with no
// per-model code — the same component serves sale.order, res.partner, product.product, …
export function ModelList() {
  const { model = '' } = useParams()
  const nav = useNavigate()
  const [params] = useSearchParams()
  const domainParam = params.get('domain')
  const filterLabel = params.get('label')
  const [contract, setContract] = useState<api.Contract | null>(null)
  const [page, setPage] = useState<api.Page | null>(null)
  const [resolve, setResolve] = useState<Resolver>(() => noResolve)
  const [error, setError] = useState<string | null>(null)
  // Live refresh: any visible change to this model (SSE hint) bumps the tick -> reload below.
  const [liveTick, setLiveTick] = useState(0)
  useEventStream(model || undefined, () => setLiveTick((t) => t + 1))

  useEffect(() => {
    let active = true
    setContract(null)
    setPage(null)
    setResolve(() => noResolve)
    setError(null)
    async function load(): Promise<void> {
      try {
        // A `?domain=` (set by smart buttons) filters the list to the related records.
        let domain: unknown
        try {
          domain = domainParam ? JSON.parse(domainParam) : undefined
        } catch {
          domain = undefined
        }
        const [c, p] = await Promise.all([api.contract(model), api.list(model, { limit: 80, domain })])
        if (!active) return
        setContract(c)
        setPage(p)
        // Resolve Many2one ids shown in columns to names (best-effort, one fetch per related model).
        const cols = c.fields.filter((f) => f.widget === 'many2one' && f.relation && c.list.columns.some((col) => col.name === f.name))
        const byModel: Record<string, { id: number; label: string }[]> = {}
        await Promise.all(
          cols.map(async (f) => {
            try {
              const rel = await api.list(f.relation as string, { limit: 200 })
              byModel[f.relation as string] = rel.data.map((r) => ({ id: r.id, label: relLabel(r) }))
            } catch {
              /* best-effort — the column falls back to #id */
            }
          }),
        )
        if (active) setResolve(() => buildResolver(byModel))
      } catch (err: unknown) {
        if (active) setError(err instanceof Error ? err.message : 'Failed to load')
      }
    }
    void load()
    return () => {
      active = false
    }
  }, [model, domainParam, liveTick])

  if (error) return <ErrorState message={error} />

  const ready = contract != null && page != null
  const cols: Column<api.Row>[] = (contract?.list.columns ?? []).map((col) => {
    const field = contract?.fields.find((f) => f.name === col.name)
    const numeric = col.widget === 'monetary' || col.widget === 'integer'
    return {
      header: col.label,
      align: numeric ? 'right' : 'left',
      mono: numeric,
      render: (row: api.Row) => displayValue(row[col.name], col.widget, field, resolve),
    }
  })

  return (
    <div>
      <PageHeader
        title={modelTitle(model)}
        subtitle={ready ? `${page!.total} record${page!.total === 1 ? '' : 's'}` : ' '}
        actions={
          <Button variant="primary" icon={<Plus size={16} />} onClick={() => nav(`/m/${model}/new`)}>
            New
          </Button>
        }
      />
      {domainParam && (
        <div className="mb-4 inline-flex items-center gap-2 rounded-full border border-accent/30 bg-accent-soft px-3 py-1 t-caption text-accent">
          <Filter size={12} />
          <span className="truncate">{filterLabel || 'Filtered'}</span>
          <button
            onClick={() => nav(`/m/${model}`)}
            aria-label="Clear filter"
            className={cx('-mr-1 grid h-4 w-4 place-items-center rounded-full hover:bg-accent/20', focusRing)}
          >
            <X size={11} />
          </button>
        </div>
      )}
      {!ready ? (
        <SkeletonTable rows={8} cols={Math.min(5, cols.length || 4)} />
      ) : page!.data.length === 0 ? (
        <div className="rounded-lg border border-dashed border-border bg-surface px-6 py-16 text-center">
          <Inbox size={28} className="mx-auto text-muted" />
          <div className="t-h2 mt-3 text-text">No {modelTitle(model).toLowerCase()} yet</div>
          <p className="t-body mt-1.5 text-muted">Create the first one to get started.</p>
          <button
            onClick={() => nav(`/m/${model}/new`)}
            className={cx('mt-5 inline-flex items-center gap-2 rounded-md bg-accent px-3.5 font-medium text-accent-fg hover:bg-accent-hover', focusRing)}
            style={{ height: 'var(--control-h)' }}
          >
            <Plus size={16} /> New
          </button>
        </div>
      ) : (
        <DataTable columns={cols} rows={page!.data} rowKey={(r) => r.id} onRowClick={(r) => nav(`/m/${model}/${r.id}`)} />
      )}
    </div>
  )
}
