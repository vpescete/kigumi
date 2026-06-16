import { Plus } from 'lucide-react'
import { customers, type Customer } from '../data'
import { Badge, Button, Column, DataTable, PageHeader } from '../ui'

export function Customers() {
  const cols: Column<Customer>[] = [
    {
      header: 'Name',
      render: (c) => (
        <div className="flex items-center gap-2.5">
          <span className="h-7 w-7 rounded-full bg-surface2 grid place-items-center text-xs font-semibold text-muted">
            {c.name.slice(0, 2).toUpperCase()}
          </span>
          <span className="font-medium text-text">{c.name}</span>
        </div>
      ),
    },
    { header: 'Email', render: (c) => <span className="text-muted">{c.email}</span> },
    { header: 'City', render: (c) => <span className="text-muted">{c.city}</span> },
    { header: 'Country', render: (c) => <Badge>{c.country}</Badge> },
    { header: 'Orders', align: 'right', mono: true, render: (c) => c.orders },
  ]
  return (
    <div>
      <PageHeader
        title="Customers"
        subtitle={`${customers.length} customers`}
        actions={<Button variant="primary" icon={<Plus size={16} />}>New customer</Button>}
      />
      <DataTable columns={cols} rows={customers} rowKey={(c) => c.id} />
    </div>
  )
}
