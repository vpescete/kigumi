import type { ReactNode } from 'react'
import { Badge } from './ui'
import type { FieldMeta, Row } from './api'

const eur = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'EUR' })
export const fmtMoney = (n: number) => eur.format(n)

// A human label for a related record (for Many2one pickers): the first present name-ish field,
// falling back to the id. Avoids per-model code while staying readable for the common models.
export function relLabel(row: Row): string {
  const r = row as Record<string, unknown>
  for (const key of ['name', 'login', 'code', 'default_code', 'title']) {
    const v = r[key]
    if (typeof v === 'string' && v.trim()) return v
  }
  return `#${row.id}`
}

/** Resolves a Many2one id to a display label (built from fetched relation records); undefined = unknown. */
export type Resolver = (model: string, id: number) => string | undefined

/** Builds an id→label resolver from related records grouped by target model. */
export function buildResolver(byModel: Record<string, { id: number; label: string }[]>): Resolver {
  const maps = new Map<string, Map<number, string>>()
  for (const [model, rows] of Object.entries(byModel)) {
    maps.set(model, new Map(rows.map((r) => [r.id, r.label])))
  }
  return (model, id) => maps.get(model)?.get(id)
}

// Renders a stored value for display, driven only by the contract's widget hint — no per-model code.
// Decimals arrive as exact JSON strings (monetary), Many2one as the related id, Selection as its key.
// A `resolve` is the optional id→name map for Many2one fields (else they fall back to a #id reference).
export function displayValue(value: unknown, widget: string, field?: FieldMeta, resolve?: Resolver): ReactNode {
  if (value === null || value === undefined || value === '') return <span className="text-muted">—</span>

  switch (widget) {
    case 'monetary':
      return fmtMoney(typeof value === 'string' ? Number.parseFloat(value) : Number(value))
    case 'integer':
      return String(value)
    case 'boolean':
      return value ? 'Yes' : 'No'
    case 'selection':
      return <SelectionBadge value={String(value)} field={field} />
    case 'many2one': {
      const label = field?.relation && resolve ? resolve(field.relation, Number(value)) : undefined
      if (label) return <span className="text-text">{label}</span>
      // No name resolution available → show the FK id as a reference.
      return <span className="text-muted">#{String(value)}</span>
    }
    case 'many2many': {
      const n = Array.isArray(value) ? value.length : 0
      return <span className="text-muted">{n === 0 ? '—' : `${n} selected`}</span>
    }
    default:
      return String(value)
  }
}

// Plain string form (for inputs / titles) — never JSX.
export function displayText(value: unknown, widget: string, field?: FieldMeta): string {
  if (value === null || value === undefined) return ''
  if (widget === 'selection') {
    const opt = field?.options?.find((o) => o.value === String(value))
    return opt ? opt.label : String(value)
  }
  if (widget === 'monetary') {
    return fmtMoney(typeof value === 'string' ? Number.parseFloat(value) : Number(value))
  }
  return String(value)
}

const STATE_TONES: Record<string, 'neutral' | 'success' | 'warning' | 'danger' | 'accent'> = {
  draft: 'neutral',
  sent: 'warning',
  sale: 'accent',
  confirmed: 'accent',
  done: 'success',
  cancel: 'danger',
  archived: 'danger',
}

export function SelectionBadge({ value, field }: { value: string; field?: FieldMeta }) {
  const label = field?.options?.find((o) => o.value === value)?.label ?? value
  const tone = STATE_TONES[value] ?? 'neutral'
  return (
    <Badge tone={tone}>
      <span className="h-1.5 w-1.5 rounded-full bg-current opacity-80" />
      {label}
    </Badge>
  )
}

// Curated, human labels per model. The contract-driven fallback (below) can't tell that sale.order and
// purchase.order both end in "order", so the colliding / important models are named explicitly here.
const MODEL_LABELS: Record<string, string> = {
  'sale.order': 'Sales Orders',
  'sale.order.line': 'Order Lines',
  'sale.order.discount': 'Discount',
  'purchase.order': 'Purchase Orders',
  'purchase.order.line': 'Purchase Lines',
  'product.product': 'Products',
  'product.template': 'Product Templates',
  'product.category': 'Categories',
  'product.pricelist': 'Pricelists',
  'product.pricelist.item': 'Pricelist Items',
  'product.attribute': 'Attributes',
  'product.attribute.value': 'Attribute Values',
  'product.template.attribute.line': 'Attribute Lines',
  'product.template.attribute.value': 'Template Attribute Values',
  'product.tag': 'Tags',
  'uom.uom': 'Units of Measure',
  'account.account': 'Chart of Accounts',
  'account.journal': 'Journals',
  'account.move': 'Journal Entries',
  'account.move.line': 'Journal Items',
  'account.tax': 'Taxes',
  'res.partner': 'Partners',
  'res.company': 'Companies',
  'res.currency': 'Currencies',
  'res.users': 'Users',
  'res.groups': 'Groups',
  'mail.message': 'Messages',
  'mail.activity': 'Activities',
  'ir.attachment': 'Attachments',
}

const titleCase = (s: string): string =>
  s
    .split(/[_.\s]+/)
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ')

// "sale.order" → "Sales Orders" (curated), else a title-cased name that includes the module when the
// last two segments differ, so colliding tails ("…​.order") don't all collapse to the same word.
export function modelTitle(model: string): string {
  if (MODEL_LABELS[model]) return MODEL_LABELS[model]
  const segs = model.split('.')
  const tail =
    segs.length >= 2 && segs[segs.length - 2] !== segs[segs.length - 1] ? segs.slice(-2).join(' ') : segs[segs.length - 1]
  return titleCase(tail)
}
