// Editable One2many grid: add / edit / remove child rows (e.g. order lines) directly on the parent
// form. On parent save, ModelForm turns the edited lines into x2many commands (create / update /
// delete) the backend applies atomically in the parent's transaction.
import { Plus, Trash2 } from 'lucide-react'
import * as api from '../api'
import { cx, focusRing } from '../ui'
import { displayValue, type Resolver } from '../format'
import { editableScalar, FieldInput, useRelOptions } from './ContractFields'

/** A line being edited: the child row's fields plus a stable client key (`id` absent on new rows). */
export type Line = { _key: string; id?: number } & Record<string, unknown>

let seq = 0
const newKey = (): string => `new-${seq++}`

/** Inlined child rows → editable lines (stable keys derived from the row id). */
export function toLines(rows: unknown): Line[] {
  return Array.isArray(rows) ? (rows as api.Row[]).map((r) => ({ _key: `row-${r.id}`, ...r })) : []
}

/** The editable scalar field names of a child contract, minus the back-reference to the parent. */
export function editableChildFields(child: api.Contract, inverse?: string): string[] {
  return child.fields.filter((f) => f.name !== inverse && editableScalar(f)).map((f) => f.name)
}

/** Diff edited lines against the originals → x2many commands (bare = create, op update/delete). */
export function toCommands(lines: Line[], original: unknown, fields: string[]): Record<string, unknown>[] {
  const orig = Array.isArray(original) ? (original as api.Row[]) : []
  const byId = new Map(orig.map((r) => [r.id, r]))
  const seen = new Set<number>()
  const cmds: Record<string, unknown>[] = []
  for (const ln of lines) {
    const values: Record<string, unknown> = {}
    for (const f of fields) if (ln[f] !== undefined && ln[f] !== '') values[f] = ln[f]
    if (ln.id == null) {
      if (Object.keys(values).length > 0) cmds.push(values) // bare object = create; skip an untouched added row
    } else {
      seen.add(ln.id)
      const before = byId.get(ln.id)
      const changed = fields.some((f) => String(ln[f] ?? '') !== String(before?.[f] ?? ''))
      if (changed) cmds.push({ op: 'update', id: ln.id, values })
    }
  }
  for (const r of orig) if (!seen.has(r.id)) cmds.push({ op: 'delete', id: r.id })
  return cmds
}

export function EditableRelation({
  field,
  child,
  lines,
  onChange,
  resolve,
}: {
  field: api.FieldMeta
  child?: api.Contract
  lines: Line[]
  onChange: (lines: Line[]) => void
  resolve?: Resolver
}) {
  const relOptions = useRelOptions(child ?? null)
  if (!child) return <div className="t-body text-muted">Loading…</div>

  // Columns from the child's curated list, minus the implied back-reference.
  const cols = child.list.columns.filter((c) => c.name !== field.inverse)
  const fieldOf = (name: string): api.FieldMeta | undefined => child.fields.find((f) => f.name === name)

  const setCell = (key: string, name: string, value: unknown): void =>
    onChange(lines.map((ln) => (ln._key === key ? { ...ln, [name]: value } : ln)))
  const remove = (key: string): void => onChange(lines.filter((ln) => ln._key !== key))
  const add = (): void => onChange([...lines, { _key: newKey() }])

  return (
    <div className="overflow-x-auto">
      <table className="w-full border-collapse text-[13px]">
        <thead>
          <tr className="border-b border-border text-left t-label text-muted">
            {cols.map((c) => (
              <th key={c.name} className="px-2.5 py-2 font-medium">{c.label}</th>
            ))}
            <th className="w-9" aria-hidden="true" />
          </tr>
        </thead>
        <tbody>
          {lines.length === 0 ? (
            <tr>
              <td colSpan={cols.length + 1} className="px-2.5 py-6 text-center text-muted">
                No {field.label.toLowerCase()} yet. Add the first one below.
              </td>
            </tr>
          ) : (
            lines.map((ln) => (
              <tr key={ln._key} className="border-b border-border/60 align-top">
                {cols.map((c) => {
                  const f = fieldOf(c.name)
                  const editable = f && editableScalar(f)
                  return (
                    <td key={c.name} className="px-2.5 py-1.5">
                      {editable ? (
                        <FieldInput
                          field={f}
                          value={ln[c.name]}
                          options={relOptions[c.name]}
                          onChange={(v) => setCell(ln._key, c.name, v)}
                        />
                      ) : (
                        <div className="py-1.5 text-text">{displayValue(ln[c.name], c.widget, f, resolve)}</div>
                      )}
                    </td>
                  )
                })}
                <td className="px-1 py-1.5">
                  <button
                    type="button"
                    onClick={() => remove(ln._key)}
                    aria-label="Remove line"
                    className={cx('rounded-md p-1.5 text-muted hover:bg-danger/10 hover:text-danger', focusRing)}
                  >
                    <Trash2 size={15} />
                  </button>
                </td>
              </tr>
            ))
          )}
        </tbody>
      </table>
      <button
        type="button"
        onClick={add}
        className={cx(
          'mt-2 inline-flex items-center gap-1.5 rounded-md border border-dashed border-border px-3 py-1.5 t-body text-muted hover:border-accent/50 hover:text-text',
          focusRing,
        )}
      >
        <Plus size={15} /> Add line
      </button>
    </div>
  )
}
