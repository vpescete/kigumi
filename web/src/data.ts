// In-memory mock data so the mockups are navigable without the Rust backend running. The shapes
// mirror the real API: a Sales Order with INLINED line items (what `find_one_secured` returns) and
// a computed `amount_total` (the aggregate compute). This is the master-detail the UI must render.

export type OrderState = 'draft' | 'sent' | 'done' | 'cancel'

export type Customer = {
  id: number
  name: string
  email: string
  city: string
  country: string
  orders: number
}

export type Product = { id: number; name: string; sku: string; price: number; stock: number }

export type OrderLine = { id: number; product: string; qty: number; price: number }

export type Order = {
  id: number
  ref: string
  customerId: number
  customer: string
  date: string
  state: OrderState
  lines: OrderLine[]
}

export const customers: Customer[] = [
  { id: 1, name: 'Acme Corporation', email: 'ap@acme.com', city: 'Milano', country: 'IT', orders: 12 },
  { id: 2, name: 'Globex SpA', email: 'orders@globex.it', city: 'Torino', country: 'IT', orders: 7 },
  { id: 3, name: 'Initech Ltd', email: 'buy@initech.co.uk', city: 'London', country: 'UK', orders: 5 },
  { id: 4, name: 'Umbrella Health', email: 'proc@umbrella.com', city: 'Berlin', country: 'DE', orders: 9 },
  { id: 5, name: 'Hooli Inc', email: 'finance@hooli.com', city: 'Palo Alto', country: 'US', orders: 3 },
  { id: 6, name: 'Stark Industries', email: 'orders@stark.com', city: 'New York', country: 'US', orders: 18 },
]

export const products: Product[] = [
  { id: 1, name: 'Consulting — Senior', sku: 'SRV-CS-01', price: 180, stock: 999 },
  { id: 2, name: 'Consulting — Junior', sku: 'SRV-CJ-01', price: 95, stock: 999 },
  { id: 3, name: 'License — Pro (seat/yr)', sku: 'LIC-PRO', price: 240, stock: 999 },
  { id: 4, name: 'License — Team (seat/yr)', sku: 'LIC-TEAM', price: 180, stock: 999 },
  { id: 5, name: 'Onboarding package', sku: 'PKG-ONB', price: 1200, stock: 50 },
  { id: 6, name: 'Support — Premium', sku: 'SUP-PREM', price: 600, stock: 999 },
]

export const orders: Order[] = [
  {
    id: 12, ref: 'S00012', customerId: 1, customer: 'Acme Corporation', date: '2026-06-09', state: 'done',
    lines: [
      { id: 1, product: 'License — Pro (seat/yr)', qty: 4, price: 240 },
      { id: 2, product: 'Onboarding package', qty: 1, price: 1200 },
      { id: 3, product: 'Support — Premium', qty: 1, price: 600 },
    ],
  },
  {
    id: 13, ref: 'S00013', customerId: 2, customer: 'Globex SpA', date: '2026-06-11', state: 'sent',
    lines: [
      { id: 4, product: 'Consulting — Senior', qty: 5, price: 180 },
      { id: 5, product: 'Consulting — Junior', qty: 2, price: 95 },
    ],
  },
  {
    id: 14, ref: 'S00014', customerId: 3, customer: 'Initech Ltd', date: '2026-06-12', state: 'draft',
    lines: [
      { id: 6, product: 'License — Team (seat/yr)', qty: 12, price: 180 },
    ],
  },
  {
    id: 15, ref: 'S00015', customerId: 4, customer: 'Umbrella Health', date: '2026-06-13', state: 'done',
    lines: [
      { id: 7, product: 'Onboarding package', qty: 2, price: 1200 },
      { id: 8, product: 'Support — Premium', qty: 1, price: 600 },
      { id: 9, product: 'Consulting — Senior', qty: 3, price: 180 },
    ],
  },
  {
    id: 16, ref: 'S00016', customerId: 6, customer: 'Stark Industries', date: '2026-06-14', state: 'sent',
    lines: [
      { id: 10, product: 'License — Pro (seat/yr)', qty: 25, price: 240 },
      { id: 11, product: 'Support — Premium', qty: 2, price: 600 },
    ],
  },
  {
    id: 17, ref: 'S00017', customerId: 5, customer: 'Hooli Inc', date: '2026-06-15', state: 'cancel',
    lines: [
      { id: 12, product: 'Consulting — Junior', qty: 4, price: 95 },
    ],
  },
]

export const lineSubtotal = (l: OrderLine) => l.qty * l.price
export const orderTotal = (o: Order) => o.lines.reduce((s, l) => s + lineSubtotal(l), 0)
export const getOrder = (id: number) => orders.find((o) => o.id === id)

const eur = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'EUR' })
export const fmtMoney = (n: number) => eur.format(n)
export const fmtDate = (iso: string) =>
  new Date(iso).toLocaleDateString('en-GB', { day: '2-digit', month: 'short', year: 'numeric' })

export const STATE_LABEL: Record<OrderState, string> = {
  draft: 'Draft',
  sent: 'Quotation Sent',
  done: 'Confirmed',
  cancel: 'Cancelled',
}
