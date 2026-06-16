import { Plus } from 'lucide-react'
import { fmtMoney, products, type Product } from '../data'
import { Badge, Button, Column, DataTable, PageHeader } from '../ui'

export function Products() {
  const cols: Column<Product>[] = [
    { header: 'Product', render: (p) => <span className="font-medium text-text">{p.name}</span> },
    { header: 'SKU', mono: true, render: (p) => <span className="text-muted">{p.sku}</span> },
    {
      header: 'Stock',
      render: (p) =>
        p.stock >= 999 ? <Badge>∞</Badge> : <Badge tone={p.stock < 60 ? 'warning' : 'neutral'}>{p.stock}</Badge>,
    },
    { header: 'Unit price', align: 'right', mono: true, render: (p) => <span className="font-medium text-text">{fmtMoney(p.price)}</span> },
  ]
  return (
    <div>
      <PageHeader
        title="Products"
        subtitle={`${products.length} products`}
        actions={<Button variant="primary" icon={<Plus size={16} />}>New product</Button>}
      />
      <DataTable columns={cols} rows={products} rowKey={(p) => p.id} />
    </div>
  )
}
