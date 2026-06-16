import { useNavigate } from 'react-router-dom'
import { CircleDollarSign, Package, ShoppingCart, Users } from 'lucide-react'
import {
  customers,
  fmtDate,
  fmtMoney,
  orders,
  orderTotal,
  products,
  type Order,
} from '../data'
import { Card, Column, DataTable, PageHeader, Stat, StateBadge } from '../ui'

const revenue = orders.filter((o) => o.state !== 'cancel').reduce((s, o) => s + orderTotal(o), 0)
const confirmed = orders.filter((o) => o.state === 'done').length
const avg = revenue / Math.max(1, orders.filter((o) => o.state !== 'cancel').length)

// Tiny inline bar chart (revenue by order) — no chart dependency.
const bars = orders.filter((o) => o.state !== 'cancel')
const maxBar = Math.max(...bars.map(orderTotal))

export function Dashboard() {
  const nav = useNavigate()
  const recent: Order[] = [...orders].slice(0, 5)

  const cols: Column<Order>[] = [
    { header: 'Reference', render: (o) => <span className="font-mono text-muted">{o.ref}</span>, width: '120px' },
    { header: 'Customer', render: (o) => <span className="font-medium">{o.customer}</span> },
    { header: 'Date', render: (o) => <span className="text-muted">{fmtDate(o.date)}</span> },
    { header: 'Status', render: (o) => <StateBadge state={o.state} /> },
    { header: 'Total', align: 'right', mono: true, render: (o) => fmtMoney(orderTotal(o)) },
  ]

  return (
    <div>
      <PageHeader title="Good morning, Valerio" subtitle="Here's what's happening across Sales today." />

      <div className="grid grid-cols-2 lg:grid-cols-4 gap-4 mb-6">
        <Stat label="Revenue (open)" value={fmtMoney(revenue)} delta={{ dir: 'up', text: '12.4% vs last week' }} icon={<CircleDollarSign size={16} />} />
        <Stat label="Orders" value={String(orders.length)} delta={{ dir: 'up', text: '3 new today' }} icon={<ShoppingCart size={16} />} />
        <Stat label="Confirmed" value={String(confirmed)} icon={<Package size={16} />} />
        <Stat label="Avg. order" value={fmtMoney(avg)} delta={{ dir: 'down', text: '1.1%' }} icon={<Users size={16} />} />
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4 mb-6">
        <Card className="p-5 lg:col-span-2">
          <div className="flex items-baseline justify-between mb-4">
            <h2 className="font-semibold text-text">Revenue by order</h2>
            <span className="text-xs text-muted">{customers.length} customers · {products.length} products</span>
          </div>
          <div className="flex items-end gap-3 h-40">
            {bars.map((o) => (
              <div key={o.id} className="flex-1 flex flex-col items-center gap-2 group">
                <div className="w-full flex items-end h-full">
                  <div
                    className="w-full rounded-t-sm bg-accent group-hover:bg-accent-hover transition-all"
                    style={{ height: `${(orderTotal(o) / maxBar) * 100}%` }}
                    title={fmtMoney(orderTotal(o))}
                  />
                </div>
                <span className="text-[11px] text-muted font-mono">{o.ref.replace('S000', '#')}</span>
              </div>
            ))}
          </div>
        </Card>

        <Card className="p-5">
          <h2 className="font-semibold text-text mb-4">Pipeline</h2>
          <div className="space-y-3">
            {(['draft', 'sent', 'done', 'cancel'] as const).map((st) => {
              const n = orders.filter((o) => o.state === st).length
              const pct = (n / orders.length) * 100
              return (
                <div key={st}>
                  <div className="flex justify-between text-xs mb-1">
                    <span className="text-muted capitalize">{st}</span>
                    <span className="text-text font-mono">{n}</span>
                  </div>
                  <div className="h-1.5 rounded-full bg-surface2 overflow-hidden">
                    <div className="h-full rounded-full bg-accent" style={{ width: `${pct}%` }} />
                  </div>
                </div>
              )
            })}
          </div>
        </Card>
      </div>

      <h2 className="font-semibold text-text mb-3">Recent orders</h2>
      <DataTable columns={cols} rows={recent} rowKey={(o) => o.id} onRowClick={(o) => nav(`/orders/${o.id}`)} />
    </div>
  )
}
