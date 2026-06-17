import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { Plus } from 'lucide-react'
import * as api from '../api'
import type { Column } from '../ui'
import { Button, DataTable, ErrorState, Loading, PageHeader } from '../ui'
import { displayValue, modelTitle } from '../format'

// A list view rendered entirely from the model's contract (columns from list.columns), with no
// per-model code — the same component serves sale.order, res.partner, product.product, …
export function ModelList() {
  const { model = '' } = useParams()
  const nav = useNavigate()
  const [contract, setContract] = useState<api.Contract | null>(null)
  const [page, setPage] = useState<api.Page | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let active = true
    setContract(null)
    setPage(null)
    setError(null)
    async function load(): Promise<void> {
      try {
        const [c, p] = await Promise.all([api.contract(model), api.list(model, { limit: 80 })])
        if (active) {
          setContract(c)
          setPage(p)
        }
      } catch (err: unknown) {
        if (active) setError(err instanceof Error ? err.message : 'Failed to load')
      }
    }
    void load()
    return () => {
      active = false
    }
  }, [model])

  if (error) return <ErrorState message={error} />
  if (!contract || !page) return <Loading />

  const cols: Column<api.Row>[] = contract.list.columns.map((col) => {
    const field = contract.fields.find((f) => f.name === col.name)
    const numeric = col.widget === 'monetary' || col.widget === 'integer'
    return {
      header: col.label,
      align: numeric ? 'right' : 'left',
      mono: numeric,
      render: (row: api.Row) => displayValue(row[col.name], col.widget, field),
    }
  })

  return (
    <div>
      <PageHeader
        title={modelTitle(model)}
        subtitle={`${page.total} record${page.total === 1 ? '' : 's'}`}
        actions={
          <Button variant="primary" icon={<Plus size={16} />} onClick={() => nav(`/m/${model}/new`)}>
            New
          </Button>
        }
      />
      {page.data.length === 0 ? (
        <div className="t-body text-muted py-16 text-center">No records yet.</div>
      ) : (
        <DataTable
          columns={cols}
          rows={page.data}
          rowKey={(r) => r.id}
          onRowClick={(r) => nav(`/m/${model}/${r.id}`)}
        />
      )}
    </div>
  )
}
