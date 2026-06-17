import type { ReactNode } from 'react'
import { fmtMoney } from './data'
import { Badge } from './ui'
import type { FieldMeta } from './api'

// Renders a stored value for display, driven only by the contract's widget hint — no per-model code.
// Decimals arrive as exact JSON strings (monetary), Many2one as the related id, Selection as its key.
export function displayValue(value: unknown, widget: string, field?: FieldMeta): ReactNode {
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
    case 'many2one':
      // Without name resolution the server returns the FK id; show it as a reference for now.
      return <span className="text-muted">#{String(value)}</span>
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

// "sale.order.line" → "Sale Order Lines" (a friendly title from a model name, last segment pluralized-ish).
export function modelTitle(model: string): string {
  const seg = model.split('.').pop() ?? model
  return seg
    .split('_')
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ')
}
