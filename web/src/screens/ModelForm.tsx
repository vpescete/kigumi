import { useCallback, useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { ArrowLeft, ChevronDown, Printer, Save } from 'lucide-react'
import * as api from '../api'
import { canRun } from '../api'
import { useAuth } from '../auth'
import type { Column } from '../ui'
import { Button, Card, confirm, cx, DataTable, ErrorState, focusRing, Loading, PageHeader, useToast } from '../ui'
import { displayValue, modelTitle } from '../format'
import { SERVICE_ACTIONS, type ServiceAction } from '../registries/serviceActions'
import { ContractFields, editableScalar, useRelOptions } from './ContractFields'
import { ReportViewer } from './ReportViewer'
import { Chatter } from './Chatter'

type FormValues = Record<string, unknown>

// Friendly labels for state actions (machine names → active-voice; falls back to a prettified name).
const ACTION_LABELS: Record<string, string> = {
  confirm: 'Confirm',
  done: 'Mark done',
  button_draft: 'Reset to draft',
  button_cancel: 'Cancel',
}
const actionLabel = (name: string): string =>
  ACTION_LABELS[name] ?? name.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase())

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
  const toast = useToast()
  const isNew = id === 'new'

  const [contract, setContract] = useState<api.Contract | null>(null)
  const [record, setRecord] = useState<api.Row | null>(null)
  const [values, setValues] = useState<FormValues>({})
  const [childContracts, setChildContracts] = useState<Record<string, api.Contract>>({})
  const relOptions = useRelOptions(contract)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [report, setReport] = useState<api.ReportMeta | null>(null)

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
      // Many2one/Many2many pickers are loaded by useRelOptions, keyed off the contract.
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

  // A cross-record service method (create_invoice, post, …): confirm if irreversible, run, toast, refresh.
  async function runService(spec: ServiceAction): Promise<void> {
    if (spec.confirm && !(await confirm({ title: spec.label, body: spec.confirm, confirmLabel: spec.label, tone: 'accent' }))) return
    setBusy(true)
    try {
      const result = await api.callEndpoint(model, Number(id), spec.endpoint)
      toast.success(spec.resultToast(result))
      await load()
    } catch (err: unknown) {
      toast.error(err instanceof api.ApiError ? err.message : `${spec.label} failed`)
    } finally {
      setBusy(false)
    }
  }

  if (error && !contract) return <ErrorState message={error} />
  if (!contract) return <Loading />

  const actions = isNew ? [] : contract.actions.filter((a) => canRun(a, identity))
  const services = isNew ? [] : SERVICE_ACTIONS[model] ?? []
  const reports = isNew ? [] : contract.reports ?? []
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
              <Button key={a.name} variant="secondary" onClick={() => act(a.name)} disabled={busy}>
                {actionLabel(a.name)}
              </Button>
            ))}
            {services.map((s) => (
              <Button key={s.endpoint} variant="secondary" onClick={() => runService(s)} disabled={busy}>
                {s.label}
              </Button>
            ))}
            {reports.length > 0 && <PrintMenu reports={reports} onPick={setReport} />}
            <Button variant="primary" icon={<Save size={16} />} onClick={save} disabled={busy}>
              {busy ? 'Saving…' : 'Save'}
            </Button>
          </>
        }
      />

      {error && <div className="t-body text-danger bg-danger-bg rounded-md px-3 py-2 mb-4">{error}</div>}
      {notice && <div className="t-body text-success bg-success-bg rounded-md px-3 py-2 mb-4">{notice}</div>}

      <Card className="p-5 mb-5">
        <ContractFields contract={contract} values={values} relOptions={relOptions} onChange={setField} />
      </Card>

      {!isNew &&
        relationFields.map((f) => (
          <InlineRelation key={f.name} field={f} rows={(record?.[f.name] as api.Row[]) ?? []} child={childContracts[f.name]} />
        ))}

      {!isNew && contract.mailed && <Chatter model={model} id={Number(id)} />}

      {report && <ReportViewer model={model} id={Number(id)} report={report} onClose={() => setReport(null)} />}
    </div>
  )
}

/** A "Print" button that opens a small menu of the model's reports. */
function PrintMenu({ reports, onPick }: { reports: api.ReportMeta[]; onPick: (r: api.ReportMeta) => void }) {
  const [open, setOpen] = useState(false)
  return (
    <div className="relative">
      <Button variant="secondary" icon={<Printer size={15} />} onClick={() => setOpen((v) => !v)}>
        Print <ChevronDown size={14} />
      </Button>
      {open && (
        <>
          <div className="fixed inset-0 z-overlay" onClick={() => setOpen(false)} aria-hidden="true" />
          <div className="absolute right-0 z-dialog mt-1 w-48 rounded-md border border-border bg-surface p-1 shadow-overlay">
            {reports.map((r) => (
              <button
                key={r.name}
                onClick={() => {
                  setOpen(false)
                  onPick(r)
                }}
                className={cx('t-body flex w-full items-center rounded-sm px-2.5 py-1.5 text-left text-text hover:bg-surface2', focusRing)}
              >
                {r.title}
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  )
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
