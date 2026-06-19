// The contract-driven field renderer shared by ModelForm and the wizard modal: the scalar-field grid,
// the per-widget FieldInput, and a hook that loads the selectable records for Many2one/Many2many pickers.
// Kept presentation-only so both the form and a wizard render fields identically.

import { useEffect, useState } from 'react'
import * as api from '../api'
import { displayValue, relLabel } from '../format'

export type RelOption = { id: number; label: string }

/** A field that can be edited and submitted (not readonly, not a One2many relation). */
export const editableScalar = (f: api.FieldMeta): boolean => !f.readonly && f.widget !== 'one2many'

/** Loads up to 200 records for each Many2one/Many2many relation so the field becomes a name picker. */
export function useRelOptions(contract: api.Contract | null): Record<string, RelOption[]> {
  const [opts, setOpts] = useState<Record<string, RelOption[]>>({})
  useEffect(() => {
    if (!contract) {
      setOpts({})
      return
    }
    let cancelled = false
    const rel = contract.fields.filter((f) => (f.widget === 'many2one' || f.widget === 'many2many') && f.relation)
    void Promise.all(
      rel.map(async (f) => {
        try {
          const page = await api.list(f.relation as string, { limit: 200 })
          return [f.name, page.data.map((r) => ({ id: r.id, label: relLabel(r) }))] as const
        } catch {
          return [f.name, [] as RelOption[]] as const
        }
      }),
    ).then((entries) => {
      if (!cancelled) setOpts(Object.fromEntries(entries))
    })
    return () => {
      cancelled = true
    }
  }, [contract])
  return opts
}

/** The scalar-field grid: a labelled input per editable field, the read-only display otherwise. */
export function ContractFields({
  contract,
  values,
  relOptions,
  onChange,
}: {
  contract: api.Contract
  values: Record<string, unknown>
  relOptions: Record<string, RelOption[]>
  onChange: (name: string, value: unknown) => void
}) {
  const scalarFields = contract.fields.filter((f) => f.widget !== 'one2many')
  return (
    <div className="grid grid-cols-1 gap-5 md:grid-cols-2">
      {scalarFields.map((f) => (
        <div key={f.name}>
          <div className="t-label text-muted mb-1.5">
            {f.label}
            {f.required && <span className="text-danger"> *</span>}
          </div>
          {f.readonly ? (
            <div className="t-body text-text py-1.5">{displayValue(values[f.name], f.widget, f)}</div>
          ) : (
            <FieldInput field={f} value={values[f.name]} options={relOptions[f.name]} onChange={(v) => onChange(f.name, v)} />
          )}
        </div>
      ))}
    </div>
  )
}

export function FieldInput({
  field,
  value,
  options,
  onChange,
}: {
  field: api.FieldMeta
  value: unknown
  options?: RelOption[]
  onChange: (value: unknown) => void
}) {
  const cls =
    'w-full px-3 rounded-md bg-surface2 border border-border text-text focus:outline-none focus:ring-2 focus:ring-[var(--color-ring)]'
  const style = { height: 'var(--control-h)' }

  switch (field.widget) {
    case 'boolean':
      return (
        <input
          type="checkbox"
          checked={Boolean(value)}
          onChange={(e) => onChange(e.target.checked)}
          className="h-4 w-4 mt-1.5 accent-[var(--color-accent)]"
        />
      )
    case 'selection':
      return (
        <select value={String(value ?? '')} onChange={(e) => onChange(e.target.value)} className={cls} style={style}>
          <option value="">—</option>
          {field.options?.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      )
    case 'integer':
    case 'monetary':
    case 'float':
      return (
        <input
          type="number"
          step={field.widget === 'integer' ? '1' : 'any'}
          value={value == null ? '' : String(value)}
          onChange={(e) => onChange(e.target.value === '' ? null : Number(e.target.value))}
          className={cls}
          style={style}
        />
      )
    case 'date':
      return (
        <input
          type="date"
          value={value == null ? '' : String(value)}
          onChange={(e) => onChange(e.target.value === '' ? null : e.target.value)}
          className={cls}
          style={style}
        />
      )
    case 'datetime':
      // The server returns "YYYY-MM-DD HH:MM:SS+TZ"; datetime-local wants "YYYY-MM-DDTHH:MM".
      return (
        <input
          type="datetime-local"
          value={value == null ? '' : String(value).replace(' ', 'T').slice(0, 16)}
          onChange={(e) => onChange(e.target.value === '' ? null : e.target.value)}
          className={cls}
          style={style}
        />
      )
    case 'many2many': {
      // SET semantics: the value is the full array of selected target ids.
      const selected = Array.isArray(value) ? (value as number[]).map(String) : []
      return (
        <select
          multiple
          value={selected}
          onChange={(e) => onChange(Array.from(e.target.selectedOptions).map((o) => Number(o.value)))}
          className={`${cls} min-h-[5rem] py-1.5`}
        >
          {(options ?? []).map((o) => (
            <option key={o.id} value={o.id}>
              {o.label}
            </option>
          ))}
        </select>
      )
    }
    case 'many2one':
      // A name picker when we have the related records; raw id input only as a fallback.
      if (options) {
        return (
          <select
            value={value == null ? '' : String(value)}
            onChange={(e) => onChange(e.target.value === '' ? null : Number(e.target.value))}
            className={cls}
            style={style}
          >
            <option value="">—</option>
            {options.map((o) => (
              <option key={o.id} value={o.id}>
                {o.label}
              </option>
            ))}
          </select>
        )
      }
      return (
        <input
          type="number"
          placeholder={field.relation ? `${field.relation} id` : 'id'}
          value={value == null ? '' : String(value)}
          onChange={(e) => onChange(e.target.value === '' ? null : Number(e.target.value))}
          className={cls}
          style={style}
        />
      )
    default:
      return (
        <input
          type="text"
          value={value == null ? '' : String(value)}
          onChange={(e) => onChange(e.target.value)}
          className={cls}
          style={style}
        />
      )
  }
}
