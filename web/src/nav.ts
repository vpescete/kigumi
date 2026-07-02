// Server-driven navigation: the sidebar is built from the models the server actually serves
// (GET /api/models), grouped by module. Internal models (lines, joins, transients, framework tables)
// are kept OUT of the sidebar to reduce noise — they stay reachable by URL and via the command palette,
// which lists every served model.

import { useEffect, useState } from 'react'
import * as api from './api'

export interface NavGroup {
  label: string
  models: string[]
}

// First matching group wins; anything unmatched falls into "Other" so nothing is silently dropped.
const GROUPS: { label: string; test: (m: string) => boolean }[] = [
  { label: 'Sales', test: (m) => m.startsWith('sale.') || m.startsWith('purchase.') || m === 'product.pricelist' },
  { label: 'Inventory', test: (m) => m.startsWith('product.') || m.startsWith('stock.') || m.startsWith('uom.') },
  { label: 'Accounting', test: (m) => m.startsWith('account.') },
  { label: 'Settings', test: (m) => m.startsWith('res.') || m.startsWith('mail.') || m.startsWith('ir.') },
]

const HIDDEN_EXACT = new Set([
  'sale.order.discount',
  'product.template.attribute.line',
  'product.template.attribute.value',
  'product.attribute.value',
  'mail.message',
  'mail.tracking',
  'mail.follower',
  'mail.activity',
  'res.groups',
  'ir.attachment',
])

/** Whether a model is too internal to surface in the sidebar (still reachable by URL / palette). */
function hidden(model: string): boolean {
  return model.endsWith('.line') || model.startsWith('ir.') || HIDDEN_EXACT.has(model)
}

/** Groups served models for the sidebar, dropping internal ones and empty groups. */
export function groupModels(models: string[]): NavGroup[] {
  const groups = GROUPS.map((g) => ({ label: g.label, models: [] as string[] }))
  const other: string[] = []
  for (const m of [...models].filter((m) => !hidden(m)).sort()) {
    const idx = GROUPS.findIndex((g) => g.test(m))
    if (idx >= 0) groups[idx].models.push(m)
    else other.push(m)
  }
  if (other.length) groups.push({ label: 'Other', models: other })
  return groups.filter((g) => g.models.length > 0)
}

/** Loads the list of served model names once. */
/** Event the Modules page fires after a live install/uninstall so the nav refetches its catalog. */
export const MODULES_CHANGED = 'kigumi:modules-changed'

export function useModels(): string[] {
  const [models, setModels] = useState<string[]>([])
  useEffect(() => {
    const refresh = (): void => void api.modelNames().then(setModels).catch(() => setModels([]))
    refresh()
    window.addEventListener(MODULES_CHANGED, refresh)
    return () => window.removeEventListener(MODULES_CHANGED, refresh)
  }, [])
  return models
}
