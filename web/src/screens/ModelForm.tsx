import { useCallback, useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { ArrowLeft, Save } from 'lucide-react'
import * as api from '../api'
import { canRun } from '../api'
import { useAuth } from '../auth'
import type { Column } from '../ui'
import { Button, Card, DataTable, ErrorState, Loading, PageHeader } from '../ui'
import { displayValue, modelTitle, relLabel } from '../format'

type FormValues = Record<string, unknown>
type RelOption = { id: number; label: string }

const editableScalar = (f: api.FieldMeta): boolean => !f.readonly && f.widget !== 'one2many'

function initialValues(contract: api.Contract, record: api.Row | null): FormValues {
  if (record) return { ...record }
  const values: FormValues = {}
  for (const f of contract.fields) {
    if (f.default !== undefined) values[f.name] = f.widget === 'boolean' ? f.default === 'true' : f.default
  }
  return values
}

// A form rendered entirely from a model's contract: editable scalar fields, the form's actions
// (filtered by the caller's groups), and any inlined One2many relation as a read-only detail table.
export function ModelForm() {
  const { model = '', id = 'new' } = useParams()
  const nav = useNavigate()
  const { identity } = useAuth()
  const isNew = id === 'new'

  const [contract, setContract] = useState<api.Contract | null>(null)
  const [record, setRecord] = useState<api.Row | null>(null)
  const [values, setValues] = useState<FormValues>({})
  const [childContracts, setChildContracts] = useState<Record<string, api.Contract>>({})
  const [relOptions, setRelOptions] = useState<Record<string, RelOption[]>>({})
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const load = useCallback(async (): Promise<void> => {
    setError(null)
    try {
      const c = await api.contract(model)
      const rec = isNew ? null : await api.getOne(model, Number(id))
      if (!isNew && !rec) {
        setError('Record not found or not permitted.')
        return
      }
      setContract(c)
      setRecord(rec)
      setValues(initialValues(c, rec))
      // Fetch the contract of each inlined One2many's target, for the detail table columns.
      const o2m = c.fields.filter((f) => f.widget === 'one2many' && f.relation)
      const children = await Promise.all(
        o2m.map(async (f) => [f.name, await api.contract(f.relation as string)] as const),
      )
      setChildContracts(Object.fromEntries(children))
      // Fetch selectable records for each Many2one/Many2many, so the field is a name picker.
      const m2o = c.fields.filter(
        (f) => (f.widget === 'many2one' || f.widget === 'many2many') && f.relation,
      )
      const opts = await Promise.all(
        m2o.map(async (f) => {
          try {
            const page = await api.list(f.relation as string, { limit: 200 })
            return [f.name, page.data.map((r) => ({ id: r.id, label: relLabel(r) }))] as const
          } catch {
            return [f.name, [] as RelOption[]] as const
          }
        }),
      )
      setRelOptions(Object.fromEntries(opts))
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load')
    }
  }, [model, id, isNew])

  useEffect(() => {
    setContract(null)
    setRecord(null)
    void load()
  }, [load])

  function setField(name: string, value: unknown): void {
    setValues((prev) => ({ ...prev, [name]: value }))
  }

  async function save(): Promise<void> {
    if (!contract) return
    setBusy(true)
    setError(null)
    try {
      const payload: FormValues = {}
      for (const f of contract.fields) {
        if (editableScalar(f) && values[f.name] !== undefined) payload[f.name] = values[f.name]
      }
      if (isNew) {
        const newId = await api.create(model, payload)
        nav(`/m/${model}/${newId}`)
      } else {
        await api.update(model, Number(id), payload)
        setNotice('Saved.')
        await load()
      }
    } catch (err: unknown) {
      setError(err instanceof api.ApiError ? err.message : 'Save failed')
    } finally {
      setBusy(false)
    }
  }

  async function act(action: string): Promise<void> {
    setBusy(true)
    setError(null)
    try {
      await api.runAction(model, Number(id), action)
      await load()
      setNotice(`Done: ${action}.`)
    } catch (err: unknown) {
      setError(err instanceof api.ApiError ? err.message : 'Action failed')
    } finally {
      setBusy(false)
    }
  }

  if (error && !contract) return <ErrorState message={error} />
  if (!contract) return <Loading />

  const actions = isNew ? [] : contract.actions.filter((a) => canRun(a, identity))
  const scalarFields = contract.fields.filter((f) => f.widget !== 'one2many')
  const relationFields = contract.fields.filter((f) => f.widget === 'one2many' && f.relation)

  return (
    <div>
      <button
        onClick={() => nav(`/m/${model}`)}
        className="inline-flex items-center gap-1.5 text-sm text-muted hover:text-text mb-4"
      >
        <ArrowLeft size={15} /> {modelTitle(model)}
      </button>

      <PageHeader
        title={isNew ? `New ${modelTitle(model)}` : (record?.name as string) || `#${id}`}
        subtitle={model}
        actions={
          <>
            {actions.map((a) => (
              <Button key={a.name} variant="secondary" onClick={() => act(a.name)}>
                {a.name}
              </Button>
            ))}
            <Button variant="primary" icon={<Save size={16} />} onClick={save}>
              {busy ? 'Saving…' : 'Save'}
            </Button>
          </>
        }
      />

      {error && <div className="t-body text-danger bg-danger-bg rounded-md px-3 py-2 mb-4">{error}</div>}
      {notice && <div className="t-body text-success bg-success-bg rounded-md px-3 py-2 mb-4">{notice}</div>}

      <Card className="p-5 mb-5">
        <div className="grid grid-cols-1 md:grid-cols-2 gap-5">
          {scalarFields.map((f) => (
            <div key={f.name}>
              <div className="t-label text-muted mb-1.5">
                {f.label}
                {f.required && <span className="text-danger"> *</span>}
              </div>
              {f.readonly ? (
                <div className="t-body text-text py-1.5">{displayValue(values[f.name], f.widget, f)}</div>
              ) : (
                <FieldInput
                  field={f}
                  value={values[f.name]}
                  options={relOptions[f.name]}
                  onChange={(v) => setField(f.name, v)}
                />
              )}
            </div>
          ))}
        </div>
      </Card>

      {!isNew &&
        relationFields.map((f) => (
          <InlineRelation key={f.name} field={f} rows={(record?.[f.name] as api.Row[]) ?? []} child={childContracts[f.name]} />
        ))}
    </div>
  )
}

function FieldInput({
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

function InlineRelation({
  field,
  rows,
  child,
}: {
  field: api.FieldMeta
  rows: api.Row[]
  child?: api.Contract
}) {
  const columns: Column<api.Row>[] = (child?.list.columns ?? [])
    .filter((c) => c.name !== field.inverse) // the back-reference to this parent is implied
    .map((c) => {
      const f = child?.fields.find((cf) => cf.name === c.name)
      const numeric = c.widget === 'monetary' || c.widget === 'integer'
      return {
        header: c.label,
        align: numeric ? ('right' as const) : ('left' as const),
        mono: numeric,
        render: (row: api.Row) => displayValue(row[c.name], c.widget, f),
      }
    })

  return (
    <div className="mb-5">
      <h2 className="t-h2 text-text mb-3">{field.label}</h2>
      {rows.length === 0 || columns.length === 0 ? (
        <div className="t-body text-muted">No lines.</div>
      ) : (
        <DataTable columns={columns} rows={rows} rowKey={(r) => r.id} />
      )}
    </div>
  )
}
