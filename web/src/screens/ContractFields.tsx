// The contract-driven field renderer shared by ModelForm and the wizard modal: the scalar-field grid,
// the per-widget FieldInput, and a hook that loads the selectable records for Many2one/Many2many pickers.
// Kept presentation-only so both the form and a wizard render fields identically.

import { useEffect, useMemo, useState } from 'react'
import * as api from '../api'
import { Combobox } from '../ui'
import { evalDomain } from '../domain'
import { buildResolver, displayValue, modelTitle, relLabel } from '../format'

export type RelOption = { id: number; label: string }

/** A field that can be edited and submitted (not readonly, not a One2many relation). */
export const editableScalar = (f: api.FieldMeta): boolean => !f.readonly && f.widget !== 'one2many'

/** Whether a field should be written on save — editable AND not dynamically readonly/invisible. */
export function isWritable(f: api.FieldMeta, values: Record<string, unknown>): boolean {
  return editableScalar(f) && !evalDomain(f.readonly_when, values) && !evalDomain(f.invisible_when, values)
}

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

/** The scalar-field grid: a labelled input per editable field, the read-only display otherwise.
 * Applies invisible_when/readonly_when live and resolves Many2one ids to names in the read-only view. */
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
  const resolve = useMemo(() => {
    const byModel: Record<string, RelOption[]> = {}
    for (const f of contract.fields) {
      if ((f.widget === 'many2one' || f.widget === 'many2many') && f.relation) byModel[f.relation] = relOptions[f.name] ?? []
    }
    return buildResolver(byModel)
  }, [contract, relOptions])

  const scalarFields = contract.fields.filter((f) => f.widget !== 'one2many')
  return (
    <div className="grid grid-cols-1 gap-5 md:grid-cols-2">
      {scalarFields.map((f) => {
        if (evalDomain(f.invisible_when, values)) return null
        const readonly = f.readonly || evalDomain(f.readonly_when, values)
        return (
          <div key={f.name}>
            <div className="t-label text-muted mb-1.5">
              {f.label}
              {f.required && <span className="text-danger"> *</span>}
            </div>
            {readonly ? (
              <div className="t-body text-text py-1.5">{displayValue(values[f.name], f.widget, f, resolve)}</div>
            ) : (
              <FieldInput field={f} value={values[f.name]} options={relOptions[f.name]} onChange={(v) => onChange(f.name, v)} />
            )}
          </div>
        )
      })}
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
      // A searchable name picker when we have the related records; raw id input only as a fallback.
      if (options) {
        return (
          <Combobox
            value={typeof value === 'number' ? value : value == null ? null : Number(value)}
            onChange={(v) => onChange(v)}
            options={options.map((o) => ({ value: o.id, label: o.label }))}
            placeholder={field.relation ? `Select ${modelTitle(field.relation)}…` : 'Select…'}
          />
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
