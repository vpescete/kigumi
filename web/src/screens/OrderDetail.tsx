import { Link, useNavigate, useParams } from 'react-router-dom'
import { ArrowLeft, Check, Plus, Printer, Trash2 } from 'lucide-react'
import { fmtDate, fmtMoney, getOrder, lineSubtotal, orderTotal } from '../data'
import { Button, Card, PageHeader, StateBadge } from '../ui'

const TAX = 0.22

export function OrderDetail() {
  const { id } = useParams()
  const nav = useNavigate()
  const order = getOrder(Number(id))

  if (!order) {
    return (
      <div>
        <Link to="/orders" className="text-accent hover:underline text-sm">← Back to orders</Link>
        <p className="mt-4 text-muted">Order not found.</p>
      </div>
    )
  }

  const subtotal = orderTotal(order)
  const tax = subtotal * TAX
  const total = subtotal + tax

  return (
    <div>
      <button
        onClick={() => nav('/orders')}
        className="inline-flex items-center gap-1.5 text-sm text-muted hover:text-text mb-4"
      >
        <ArrowLeft size={15} /> Sales Orders
      </button>

      <PageHeader
        title={order.ref}
        subtitle={`${order.customer} · ${fmtDate(order.date)}`}
        actions={
          <>
            <Button variant="ghost" icon={<Printer size={16} />}>Print</Button>
            <Button variant="secondary" icon={<Trash2 size={16} />}>Cancel</Button>
            <Button variant="primary" icon={<Check size={16} />}>Confirm</Button>
          </>
        }
      />

      {/* Header / master record */}
      <Card className="p-5 mb-5">
        <div className="grid grid-cols-2 md:grid-cols-4 gap-5">
          <Field label="Customer" value={order.customer} />
          <Field label="Order date" value={fmtDate(order.date)} />
          <div>
            <div className="text-[11px] uppercase tracking-wide text-muted mb-1.5">Status</div>
            <StateBadge state={order.state} />
          </div>
          <Field label="Total" value={fmtMoney(total)} mono accent />
        </div>
      </Card>

      {/* Detail / inlined line items */}
      <div className="flex items-center justify-between mb-3">
        <h2 className="t-h2 text-text">Order lines</h2>
        <Button variant="ghost" icon={<Plus size={16} />}>Add a line</Button>
      </div>

      <Card className="overflow-hidden">
        <table className="w-full border-collapse">
          <thead>
            <tr className="border-b border-border text-muted">
              <th className="t-label text-left px-4 py-2.5">Product</th>
              <th className="t-label text-right px-4 py-2.5 w-20">Qty</th>
              <th className="t-label text-right px-4 py-2.5 w-32">Unit price</th>
              <th className="t-label text-right px-4 py-2.5 w-32">Subtotal</th>
            </tr>
          </thead>
          <tbody>
            {order.lines.map((l) => (
              <tr
                key={l.id}
                className="border-b border-border last:border-0 hover:bg-surface2"
                style={{ height: 'var(--density-row)' }}
              >
                <td className="px-4 t-body text-text font-medium">{l.product}</td>
                <td className="px-4 text-right t-mono text-muted">{l.qty}</td>
                <td className="px-4 text-right t-mono text-muted">{fmtMoney(l.price)}</td>
                <td className="px-4 text-right t-mono text-text font-medium">{fmtMoney(lineSubtotal(l))}</td>
              </tr>
            ))}
          </tbody>
        </table>

        {/* Totals footer — amount_total is the aggregate compute over the inlined lines */}
        <div className="flex justify-end border-t border-border bg-surface2 px-4 py-4">
          <div className="w-64 space-y-2">
            <Row label="Untaxed" value={fmtMoney(subtotal)} />
            <Row label="Tax (22%)" value={fmtMoney(tax)} muted />
            <div className="border-t border-border pt-2 flex justify-between items-baseline">
              <span className="t-body font-semibold text-text">Total</span>
              <span className="font-mono tabular-nums text-lg font-semibold text-accent">
                {fmtMoney(total)}
              </span>
            </div>
          </div>
        </div>
      </Card>
    </div>
  )
}

function Field({
  label,
  value,
  mono,
  accent,
}: {
  label: string
  value: string
  mono?: boolean
  accent?: boolean
}) {
  return (
    <div>
      <div className="t-label text-muted mb-1.5">{label}</div>
      <div
        className={
          (mono ? 't-mono ' : 't-subtitle ') +
          'font-medium ' +
          (accent ? 'text-accent' : 'text-text')
        }
      >
        {value}
      </div>
    </div>
  )
}

function Row({ label, value, muted }: { label: string; value: string; muted?: boolean }) {
  return (
    <div className="flex justify-between">
      <span className="t-body text-muted">{label}</span>
      <span className={'t-mono ' + (muted ? 'text-muted' : 'text-text')}>{value}</span>
    </div>
  )
}
