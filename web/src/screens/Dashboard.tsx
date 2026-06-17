import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { Package, ShoppingCart, Users } from 'lucide-react'
import * as api from '../api'
import type { Column } from '../ui'
import { DataTable, Loading, PageHeader, Stat } from '../ui'
import { displayValue } from '../format'

const PRIMARY = [
  { model: 'sale.order', label: 'Sales Orders', icon: ShoppingCart },
  { model: 'res.partner', label: 'Customers', icon: Users },
  { model: 'product.product', label: 'Products', icon: Package },
] as const

export function Dashboard() {
  const nav = useNavigate()
  const [counts, setCounts] = useState<Record<string, number> | null>(null)
  const [orders, setOrders] = useState<api.Page | null>(null)
  const [contract, setContract] = useState<api.Contract | null>(null)

  useEffect(() => {
    let active = true
    async function load(): Promise<void> {
      try {
        const pages = await Promise.all(PRIMARY.map((p) => api.list(p.model, { limit: 1 })))
        const c: Record<string, number> = {}
        PRIMARY.forEach((p, i) => {
          c[p.model] = pages[i].total
        })
        const [recent, oc] = await Promise.all([
          api.list('sale.order', { limit: 8, order: '-id' }),
          api.contract('sale.order'),
        ])
        if (active) {
          setCounts(c)
          setOrders(recent)
          setContract(oc)
        }
      } catch {
        if (active) setCounts({}) // render what we can; tiles show 0
      }
    }
    void load()
    return () => {
      active = false
    }
  }, [])

  if (!counts) return <Loading />

  const recentCols: Column<api.Row>[] = (contract?.list.columns ?? []).slice(0, 5).map((col) => {
    const f = contract?.fields.find((ff) => ff.name === col.name)
    const numeric = col.widget === 'monetary' || col.widget === 'integer'
    return {
      header: col.label,
      align: numeric ? 'right' : 'left',
      mono: numeric,
      render: (r: api.Row) => displayValue(r[col.name], col.widget, f),
    }
  })

  return (
    <div>
      <PageHeader title="Dashboard" subtitle="Live overview" />
      <div className="grid grid-cols-1 sm:grid-cols-3 gap-4 mb-7">
        {PRIMARY.map((p) => (
          <button key={p.model} onClick={() => nav(`/m/${p.model}`)} className="text-left">
            <Stat label={p.label} value={String(counts[p.model] ?? 0)} icon={<p.icon size={16} />} />
          </button>
        ))}
      </div>

      {orders && orders.data.length > 0 && (
        <>
          <h2 className="t-h2 text-text mb-3">Latest orders</h2>
          <DataTable
            columns={recentCols}
            rows={orders.data}
            rowKey={(r) => r.id}
            onRowClick={(r) => nav(`/m/sale.order/${r.id}`)}
          />
        </>
      )}
    </div>
  )
}
