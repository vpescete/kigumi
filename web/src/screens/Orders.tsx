import { useNavigate } from 'react-router-dom'
import { Plus, SlidersHorizontal } from 'lucide-react'
import { fmtDate, fmtMoney, orders, orderTotal, type Order } from '../data'
import { Button, Column, DataTable, PageHeader, StateBadge } from '../ui'

export function Orders() {
  const nav = useNavigate()

  const cols: Column<Order>[] = [
    { header: 'Reference', width: '120px', mono: true, render: (o) => <span className="text-muted">{o.ref}</span> },
    { header: 'Customer', render: (o) => <span className="font-medium text-text">{o.customer}</span> },
    { header: 'Date', render: (o) => <span className="text-muted">{fmtDate(o.date)}</span> },
    { header: 'Lines', align: 'right', mono: true, render: (o) => <span className="text-muted">{o.lines.length}</span> },
    { header: 'Status', render: (o) => <StateBadge state={o.state} /> },
    { header: 'Total', align: 'right', mono: true, render: (o) => <span className="font-medium">{fmtMoney(orderTotal(o))}</span> },
  ]

  return (
    <div>
      <PageHeader
        title="Sales Orders"
        subtitle={`${orders.length} orders`}
        actions={
          <>
            <Button variant="ghost" icon={<SlidersHorizontal size={16} />}>Filters</Button>
            <Button variant="primary" icon={<Plus size={16} />}>New order</Button>
          </>
        }
      />
      <DataTable columns={cols} rows={orders} rowKey={(o) => o.id} onRowClick={(o) => nav(`/orders/${o.id}`)} />
    </div>
  )
}
