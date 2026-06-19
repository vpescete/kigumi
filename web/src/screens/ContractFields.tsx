// The contract-driven field renderer shared by ModelForm and the wizard modal: the scalar-field grid,
// the per-widget FieldInput, and a hook that loads the selectable records for Many2one/Many2many pickers.
// Kept presentation-only so both the form and a wizard render fields identically.

import { useEffect, useMemo, useState } from 'react'
import * as api from '../api'
import { Combobox } from '../ui'
import { evalDomain } from '../domain'
import { buildResolver, displayValue, modelTitle, relLabel, type Resolver } from '../format'

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

/** Wide widgets span both columns so they never sit lopsided next to a small input. */
const isWide = (f: api.FieldMeta): boolean => f.widget === 'many2many' || f.widget === 'html' || f.widget === 'image'

/** Builds the id→label resolver from the fetched relation options (null contract → resolves nothing). */
export function useResolver(contract: api.Contract | null, relOptions: Record<string, RelOption[]>): Resolver {
  return useMemo(() => {
    const byModel: Record<string, RelOption[]> = {}
    for (const f of contract?.fields ?? []) {
      if ((f.widget === 'many2one' || f.widget === 'many2many') && f.relation) byModel[f.relation] = relOptions[f.name] ?? []
    }
    return buildResolver(byModel)
  }, [contract, relOptions])
}

type Slot = { field: api.FieldMeta; full: boolean }
type SheetGroup = { title: string | null; slots: Slot[] }

/** The scalar-field groups for the sheet: the model's declared view, or a smart default (name first,
 * wide widgets full-width) when none. Required scalar fields the view forgot are appended to "Other". */
function sheetGroups(contract: api.Contract): SheetGroup[] {
  const byName = new Map(contract.fields.map((f) => [f.name, f]))
  const view = contract.view
  if (view && view.groups.length) {
    const placed = new Set<string>()
    view.groups.forEach((g) => g.fields.forEach((s) => placed.add(s.name)))
    view.pages.forEach((p) => p.fields.forEach((n) => placed.add(n)))
    const groups: SheetGroup[] = view.groups
      .map((g) => ({
        title: g.title,
        slots: g.fields
          .map((s) => {
            const f = byName.get(s.name)
            return f && f.widget !== 'one2many' ? { field: f, full: s.full } : null
          })
          .filter((s): s is Slot => s !== null),
      }))
      .filter((g) => g.slots.length > 0)
    const orphans = contract.fields.filter((f) => f.widget !== 'one2many' && f.required && !f.readonly && !placed.has(f.name))
    if (orphans.length) groups.push({ title: 'Other', slots: orphans.map((f) => ({ field: f, full: isWide(f) })) })
    return groups
  }
  // Fallback: one group, the primary name first, wide widgets full-width.
  const scalar = contract.fields.filter((f) => f.widget !== 'one2many')
  scalar.sort((a, b) => (a.name === 'name' ? -1 : 0) - (b.name === 'name' ? -1 : 0))
  return [{ title: null, slots: scalar.map((f) => ({ field: f, full: isWide(f) })) }]
}

/** The notebook pages (tabs): the model's declared pages, or a single "Details" tab of its One2many. */
export function notebookPages(contract: api.Contract): api.ViewPage[] {
  const view = contract.view
  if (view && view.pages.length) return view.pages
  const o2m = contract.fields.filter((f) => f.widget === 'one2many' && f.relation).map((f) => f.name)
  return o2m.length ? [{ title: 'Details', fields: o2m }] : []
}

/** A single labelled field: read-only display (resolving Many2one to a name) or an editable input,
 * honoring invisible_when / readonly_when. Returns null when the field is dynamically invisible. */
export function FieldCell({
  field,
  values,
  relOptions,
  resolve,
  onChange,
  full,
}: {
  field: api.FieldMeta
  values: Record<string, unknown>
  relOptions: Record<string, RelOption[]>
  resolve: Resolver
  onChange: (name: string, value: unknown) => void
  full?: boolean
}) {
  if (evalDomain(field.invisible_when, values)) return null
  const readonly = field.readonly || evalDomain(field.readonly_when, values)
  return (
    <div className={full ? 'md:col-span-2' : ''}>
      <div className="t-label text-muted mb-1.5">
        {field.label}
        {field.required && <span className="text-danger"> *</span>}
      </div>
      {readonly ? (
        <div className="t-body text-text py-1.5">{displayValue(values[field.name], field.widget, field, resolve)}</div>
      ) : (
        <FieldInput field={field} value={values[field.name]} options={relOptions[field.name]} onChange={(v) => onChange(field.name, v)} />
      )}
    </div>
  )
}

/** The form sheet: scalar-field groups laid out from the model's view (or a smart default). */
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
  const resolve = useResolver(contract, relOptions)
  const groups = useMemo(() => sheetGroups(contract), [contract])
  return (
    <div className="space-y-7">
      {groups.map((g, gi) => (
        <section key={gi}>
          {g.title && (
            <div className="mb-3 flex items-center gap-2">
              <span className="h-3 w-0.5 rounded-full bg-accent" aria-hidden="true" />
              <h3 className="t-label text-text">{g.title}</h3>
            </div>
          )}
          <div className="grid grid-cols-1 gap-x-5 gap-y-4 md:grid-cols-2">
            {g.slots.map((s) => (
              <FieldCell key={s.field.name} field={s.field} values={values} relOptions={relOptions} resolve={resolve} onChange={onChange} full={s.full} />
            ))}
          </div>
        </section>
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
