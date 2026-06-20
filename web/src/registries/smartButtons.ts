// Smart buttons (Odoo-style): per-model stat shortcuts shown on a record form. Each surfaces a number
// — a live count of related records, or a value read off the record — and links to the filtered list.
// A client registry for now (the seam until the contract advertises them), like wizards/serviceActions.
import type { ReactNode } from 'react'
import { Boxes, FileText, Package, ShoppingCart, type LucideIcon } from 'lucide-react'
import { createElement } from 'react'

export type SmartButton = {
  label: string
  icon: ReactNode
  // The number shown: a live count of `target` records whose `field` points at this record, OR a value
  // read off the record itself.
  source: { kind: 'count'; target: string; field: string } | { kind: 'field'; name: string; widget?: string }
  // Where clicking navigates (a domain-filtered list). Defaults to the counted relation for `count`.
  link?: { target: string; field: string }
}

const icon = (c: LucideIcon): ReactNode => createElement(c, { size: 16 })

export const SMART_BUTTONS: Record<string, SmartButton[]> = {
  'res.partner': [
    { label: 'Sales Orders', icon: icon(ShoppingCart), source: { kind: 'count', target: 'sale.order', field: 'partner_id' } },
    { label: 'Purchase Orders', icon: icon(Package), source: { kind: 'count', target: 'purchase.order', field: 'partner_id' } },
    { label: 'Invoices', icon: icon(FileText), source: { kind: 'count', target: 'account.move', field: 'partner_id' } },
  ],
  'product.product': [
    {
      label: 'On Hand',
      icon: icon(Boxes),
      source: { kind: 'field', name: 'qty_available', widget: 'float' },
      link: { target: 'stock.quant', field: 'product_id' },
    },
  ],
}
