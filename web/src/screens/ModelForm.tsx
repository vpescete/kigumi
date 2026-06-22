import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useBlocker, useNavigate, useParams } from 'react-router-dom'
import { ArrowLeft, Eye, EyeOff, Plus, Printer, Save, SlidersHorizontal, Wand2 } from 'lucide-react'
import * as api from '../api'
import { canRun } from '../api'
import { useAuth } from '../auth'
import type { Column } from '../ui'
import { Button, Card, confirm, DataTable, Dialog, ErrorState, Menu, type MenuGroup, PageHeader, Skeleton, Tabs, useToast } from '../ui'
import { buildResolver, displayValue, modelTitle, relLabel, type Resolver } from '../format'
import { SERVICE_ACTIONS, type ServiceAction } from '../registries/serviceActions'
import { EditableRelation, editableChildFields, toCommands, toLines, type Line } from './EditableRelation'
import { SmartButtons } from './SmartButtons'
import { ContractFields, FieldCell, isWritable, notebookPages, useRelOptions, useResolver } from './ContractFields'
import { ReportViewer } from './ReportViewer'
import { WizardModal } from './WizardModal'
import { WIZARDS, type WizardSpec } from '../registries/wizards'
import { Chatter } from './Chatter'
import { AddFieldDialog } from './AddFieldDialog'

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
  const values: FormValues = record ? { ...record } : {}
  for (const f of contract.fields) {
    if (f.widget === 'one2many') {
      // One2many fields are held as editable lines (stable keys), not raw rows.
      values[f.name] = toLines(record?.[f.name])
    } else if (!record && f.default !== undefined) {
      values[f.name] = f.widget === 'boolean' ? f.default === 'true' : f.default
    }
  }
  return values
}

// A form rendered entirely from a model's contract: editable scalar fields, the form's actions
// (filtered by the caller's groups), and any inlined One2many relation as a read-only detail table.
export function ModelForm() {
  const { model = '', id = 'new' } = useParams()
  const nav = useNavigate()
  const { identity } = useAuth()
  const isAdmin = identity?.groups.includes('admin') ?? false
  const toast = useToast()
  const isNew = id === 'new'

  const [contract, setContract] = useState<api.Contract | null>(null)
  const [record, setRecord] = useState<api.Row | null>(null)
  const [values, setValues] = useState<FormValues>({})
  // Snapshot of the values at load — the baseline for the dirty check (handles new-record defaults:
  // an untouched new form equals its defaults, so it is not "dirty").
  const [initial, setInitial] = useState<FormValues>({})
  const [childContracts, setChildContracts] = useState<Record<string, api.Contract>>({})
  const relOptions = useRelOptions(contract)
  const resolve = useResolver(contract, relOptions)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [report, setReport] = useState<api.ReportMeta | null>(null)
  const [wizard, setWizard] = useState<WizardSpec | null>(null)
  const [addFieldOpen, setAddFieldOpen] = useState(false)
  const [customizing, setCustomizing] = useState(false)
  // Hidden fields (dropped from the contract) and the field being relabeled — both only used in Customize.
  const [hidden, setHidden] = useState<api.ViewOverrideRow[]>([])
  const [relabelTarget, setRelabelTarget] = useState<{ field: string; label: string } | null>(null)
  // When true, the next in-app navigation is NOT blocked (used for the post-save redirect).
  const skipGuardRef = useRef(false)
  // Serializes Customize toggles: a setView + refetch must finish before the next, or two concurrent
  // load() calls race on setContract.
  const customizeBusyRef = useRef(false)

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
      const init = initialValues(c, rec)
      setValues(init)
      setInitial(init)
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

  // Studio: persist a per-field view override (lock / hide / relabel) and refetch the contract + the
  // hidden-fields list so the change shows. Layout metadata, not record data — never touches save.
  // Serialized behind a ref so rapid toggles cannot race two load() refetches.
  const applyOverride = useCallback(
    async (field: string, patch: { readonly?: boolean; invisible?: boolean; label?: string }): Promise<void> => {
      if (customizeBusyRef.current) return
      customizeBusyRef.current = true
      try {
        await api.setView(model, { field, ...patch })
        await load()
        setHidden((await api.viewOverrides(model)).filter((o) => o.invisible))
      } catch (e: unknown) {
        toast.error(e instanceof api.ApiError ? e.message : 'Customize failed')
      } finally {
        customizeBusyRef.current = false
      }
    },
    [model, load, toast],
  )

  // Load the hidden-fields list when Customize opens (the contract drops hidden fields, so they need a
  // separate fetch to be shown/un-hidden); clear it when Customize closes or the model changes.
  useEffect(() => {
    if (!customizing) {
      setHidden([])
      return
    }
    let cancelled = false
    void api
      .viewOverrides(model)
      .then((rows) => {
        if (!cancelled) setHidden(rows.filter((o) => o.invisible))
      })
      .catch(() => {
        if (!cancelled) setHidden([])
      })
    return () => {
      cancelled = true
    }
  }, [customizing, model])

  useEffect(() => {
    skipGuardRef.current = false // re-arm the nav guard for the freshly loaded record
    setContract(null)
    setRecord(null)
    void load()
  }, [load])

  // Unsaved-changes signal: current values differ from the load snapshot (scalars vs `initial`,
  // One2many via its x2many command diff vs the loaded rows). Used by the dirty-aware Save AND the
  // navigation guard below.
  const dirty = useMemo(() => {
    if (!contract) return false
    return contract.fields.some((f) => {
      if (f.widget === 'one2many' && f.relation) {
        const child = childContracts[f.name]
        return child
          ? toCommands((values[f.name] as Line[]) ?? [], record?.[f.name], editableChildFields(child, f.inverse)).length > 0
          : false
      }
      if (!isWritable(f, values)) return false
      return String(values[f.name] ?? '') !== String(initial[f.name] ?? '')
    })
  }, [contract, values, initial, record, childContracts])

  // Browser-level guard: warn before a tab close / reload / external navigation while there are
  // unsaved changes.
  useEffect(() => {
    if (!dirty) return
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault()
      e.returnValue = ''
    }
    window.addEventListener('beforeunload', onBeforeUnload)
    return () => window.removeEventListener('beforeunload', onBeforeUnload)
  }, [dirty])

  // In-app guard: block an SPA navigation away from a dirty form and ask to confirm. `skipGuardRef`
  // lets the post-save navigation (a new record redirecting to its id) pass without prompting.
  const blocker = useBlocker(
    ({ currentLocation, nextLocation }) => dirty && !skipGuardRef.current && currentLocation.pathname !== nextLocation.pathname,
  )
  useEffect(() => {
    if (blocker.state !== 'blocked') return
    void confirm({
      title: 'Discard unsaved changes?',
      body: 'You have unsaved changes on this record. Leave without saving them?',
      confirmLabel: 'Discard changes',
      tone: 'danger',
    }).then((ok) => (ok ? blocker.proceed() : blocker.reset()))
  }, [blocker])

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
        if (f.widget === 'one2many' && f.relation) {
          const child = childContracts[f.name]
          if (!child) continue
          const cmds = toCommands(
            (values[f.name] as Line[]) ?? [],
            isNew ? [] : record?.[f.name],
            editableChildFields(child, f.inverse),
          )
          if (cmds.length) payload[f.name] = cmds
        } else if (isWritable(f, values) && values[f.name] !== undefined) {
          payload[f.name] = values[f.name]
        }
      }
      if (isNew) {
        const newId = await api.create(model, payload)
        toast.success(`${modelTitle(model)} created`)
        skipGuardRef.current = true // the create redirect is not an "abandon changes" navigation
        nav(`/m/${model}/${newId}`)
      } else {
        await api.update(model, Number(id), payload)
        toast.success('Changes saved')
        await load()
      }
    } catch (err: unknown) {
      toast.error(err instanceof api.ApiError ? err.message : 'Save failed')
    } finally {
      setBusy(false)
    }
  }

  async function act(action: string): Promise<void> {
    setBusy(true)
    try {
      await api.runAction(model, Number(id), action)
      await load()
      toast.success(`${actionLabel(action)} done`)
    } catch (err: unknown) {
      toast.error(err instanceof api.ApiError ? err.message : `${actionLabel(action)} failed`)
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
  if (!contract)
    return (
      <div>
        <Skeleton w="14rem" h="2rem" className="mb-6" />
        <Card className="p-5">
          <div className="grid grid-cols-1 gap-5 md:grid-cols-2">
            {Array.from({ length: 6 }).map((_, i) => (
              <div key={i}>
                <Skeleton w="6rem" h="0.7em" className="mb-2" />
                <Skeleton w="100%" h="var(--control-h)" />
              </div>
            ))}
          </div>
        </Card>
      </div>
    )

  const actions = isNew ? [] : contract.actions.filter((a) => canRun(a, identity))
  const services = isNew ? [] : SERVICE_ACTIONS[model] ?? []
  const wizards = isNew ? [] : WIZARDS[model] ?? []
  const reports = isNew ? [] : contract.reports ?? []
  const pages = isNew ? [] : notebookPages(contract)

  // Operations menu: service methods + wizards + reports, grouped. `ops` is the flat list (for the
  // single-op inline case), `opGroups` the grouped list (for the Actions menu).
  const opGroups: MenuGroup[] = [
    { label: 'Operations', items: services.map((s) => ({ label: s.label, onSelect: () => void runService(s) })) },
    { label: 'Tools', items: wizards.map((w) => ({ label: w.label, onSelect: () => setWizard(w) })) },
    { label: 'Print', items: reports.map((r) => ({ label: r.title, icon: <Printer size={14} />, onSelect: () => setReport(r) })) },
  ].filter((g) => g.items.length > 0)
  const ops = opGroups.flatMap((g) => g.items)

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
            {/* Unsaved-changes hint: explains why Save is enabled (and, when absent, why it is greyed). */}
            {dirty && !isNew && (
              <span className="mr-1 inline-flex items-center gap-1.5 t-caption text-warning">
                <span className="h-1.5 w-1.5 rounded-full bg-current" aria-hidden="true" />
                Unsaved changes
              </span>
            )}
            {/* Workflow transitions: visible, secondary emphasis (Save stays the one primary). */}
            {actions.map((a) => (
              <Button key={a.name} variant="outline" onClick={() => act(a.name)} disabled={busy}>
                {actionLabel(a.name)}
              </Button>
            ))}
            {/* Everything you "run" on the record — operations, tools, print — collapses into one menu
                (or a single inline button when there is exactly one), instead of a row of flat buttons. */}
            {ops.length === 1 ? (
              <Button variant="outline" icon={ops[0].icon} onClick={ops[0].onSelect} disabled={busy}>
                {ops[0].label}
              </Button>
            ) : ops.length > 1 ? (
              <Menu label="Actions" icon={<SlidersHorizontal size={15} />} groups={opGroups} disabled={busy} />
            ) : null}
            {/* Studio: extend the model itself (admin only) — adds a real column at runtime, no recompile. */}
            {isAdmin && (
              <Button variant="outline" icon={<Plus size={16} />} onClick={() => setAddFieldOpen(true)} disabled={busy}>
                Add field
              </Button>
            )}
            {/* Studio: customize the form layout (lock/unlock fields) live. */}
            {isAdmin && (
              <Button
                variant={customizing ? 'primary' : 'outline'}
                icon={<Wand2 size={16} />}
                onClick={() => setCustomizing((v) => !v)}
                disabled={busy}
              >
                {customizing ? 'Done' : 'Customize'}
              </Button>
            )}
            <Button variant="primary" icon={<Save size={16} />} onClick={save} disabled={busy || !dirty}>
              {busy ? 'Saving…' : 'Save'}
            </Button>
          </>
        }
      />

      {!isNew && <SmartButtons model={model} recordId={Number(id)} record={record} />}

      <Card className="p-5 mb-5">
        <ContractFields
          contract={contract}
          values={values}
          relOptions={relOptions}
          onChange={setField}
          context={{ model, recordId: isNew ? null : Number(id) }}
          customize={
            isAdmin && customizing
              ? {
                  onSetReadonly: (f, ro) => void applyOverride(f, { readonly: ro }),
                  onHide: (f) => void applyOverride(f, { invisible: true }),
                  onRelabel: (f, label) => setRelabelTarget({ field: f, label }),
                }
              : undefined
          }
        />
      </Card>

      {isAdmin && customizing && hidden.length > 0 && (
        <Card className="mb-5 p-4">
          <div className="t-label text-muted mb-2.5 flex items-center gap-1.5">
            <EyeOff size={13} /> Hidden fields — click to show
          </div>
          <div className="flex flex-wrap gap-2">
            {hidden.map((h) => (
              <button
                key={h.field}
                type="button"
                onClick={() => void applyOverride(h.field, { invisible: false })}
                className="inline-flex items-center gap-1.5 rounded-md border border-border bg-surface2 px-2.5 py-1 text-sm text-text transition-colors hover:border-accent/40"
              >
                <Eye size={13} className="text-muted" /> {h.label || h.field}
              </button>
            ))}
          </div>
        </Card>
      )}

      {!isNew && pages.length > 0 && (
        <Card className="mb-5 p-5">
          <Tabs
            tabs={pages.map((p) => ({
              id: p.title,
              label: p.title,
              content: (
                <PageContent
                  page={p}
                  contract={contract}
                  record={record}
                  childContracts={childContracts}
                  values={values}
                  relOptions={relOptions}
                  resolve={resolve}
                  onChange={setField}
                  context={{ model, recordId: isNew ? null : Number(id) }}
                />
              ),
            }))}
          />
        </Card>
      )}

      {!isNew && contract.mailed && <Chatter model={model} id={Number(id)} />}

      {report && <ReportViewer model={model} id={Number(id)} report={report} onClose={() => setReport(null)} />}
      {wizard && <WizardModal spec={wizard} hostModel={model} hostId={Number(id)} onClose={() => setWizard(null)} onApplied={load} />}
      {addFieldOpen && (
        <AddFieldDialog
          model={model}
          existing={contract.fields.map((f) => f.name)}
          onClose={() => setAddFieldOpen(false)}
          onAdded={async () => {
            setAddFieldOpen(false)
            await load()
          }}
        />
      )}
      {relabelTarget && (
        <RelabelDialog
          target={relabelTarget}
          onClose={() => setRelabelTarget(null)}
          onSubmit={(label) => {
            const field = relabelTarget.field
            setRelabelTarget(null)
            void applyOverride(field, { label })
          }}
        />
      )}
    </div>
  )
}

/** A one-field dialog to relabel a field's UI caption (Studio Customize). */
function RelabelDialog({
  target,
  onClose,
  onSubmit,
}: {
  target: { field: string; label: string }
  onClose: () => void
  onSubmit: (label: string) => void
}) {
  const [label, setLabel] = useState(target.label)
  const trimmed = label.trim()
  return (
    <Dialog
      open
      onClose={onClose}
      title={`Relabel "${target.field}"`}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => onSubmit(trimmed)} disabled={!trimmed || trimmed === target.label}>
            Relabel
          </Button>
        </>
      }
    >
      <label className="block">
        <span className="t-caption mb-1.5 block text-muted">Label</span>
        <input
          className="w-full rounded-md border border-input-border bg-input px-3 text-text shadow-xs focus:outline-none focus-visible:border-accent focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:shadow-focus"
          style={{ height: 'var(--control-h)' }}
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          autoFocus
          onKeyDown={(e) => {
            if (e.key === 'Enter' && trimmed && trimmed !== target.label) onSubmit(trimmed)
          }}
        />
      </label>
    </Dialog>
  )
}

/** A notebook page: renders each of its fields — One2many as an inline table, anything else as a
 * full-width field cell (e.g. a Many2many of attribute values). */
function PageContent({
  page,
  contract,
  record,
  childContracts,
  values,
  relOptions,
  resolve,
  onChange,
  context,
}: {
  page: api.ViewPage
  contract: api.Contract
  record: api.Row | null
  childContracts: Record<string, api.Contract>
  values: FormValues
  relOptions: Record<string, import('./ContractFields').RelOption[]>
  resolve: Resolver
  onChange: (name: string, value: unknown) => void
  context: import('./ContractFields').FieldContext
}) {
  return (
    <div className="space-y-6">
      {page.fields.map((name) => {
        const f = contract.fields.find((ff) => ff.name === name)
        if (!f) return null
        if (f.widget === 'one2many') {
          // Editable grid (add/edit/remove lines) unless the relation is read-only.
          return f.readonly ? (
            <InlineRelation key={name} field={f} rows={(record?.[name] as api.Row[]) ?? []} child={childContracts[name]} resolve={resolve} />
          ) : (
            <EditableRelation
              key={name}
              field={f}
              child={childContracts[name]}
              lines={(values[name] as Line[]) ?? []}
              onChange={(lines) => onChange(name, lines)}
              resolve={resolve}
            />
          )
        }
        return (
          <div key={name} className="grid grid-cols-1 md:grid-cols-2">
            <FieldCell field={f} values={values} relOptions={relOptions} resolve={resolve} onChange={onChange} full context={context} />
          </div>
        )
      })}
    </div>
  )
}

function InlineRelation({
  field,
  rows,
  child,
  resolve,
}: {
  field: api.FieldMeta
  rows: api.Row[]
  child?: api.Contract
  resolve?: Resolver
}) {
  // Resolve the child's own Many2one columns to names (e.g. a line's product), independent of the
  // parent's resolver — best-effort, one fetch per related model (mirrors the list view).
  const [childResolve, setChildResolve] = useState<Resolver>(() => () => undefined)
  useEffect(() => {
    let active = true
    async function load(): Promise<void> {
      if (!child) return
      const cols = child.fields.filter(
        (f) => f.widget === 'many2one' && f.relation && f.name !== field.inverse && child.list.columns.some((c) => c.name === f.name),
      )
      const byModel: Record<string, { id: number; label: string }[]> = {}
      await Promise.all(
        cols.map(async (f) => {
          try {
            const rel = await api.list(f.relation as string, { limit: 200 })
            byModel[f.relation as string] = rel.data.map((r) => ({ id: r.id, label: relLabel(r) }))
          } catch {
            /* best-effort — the column falls back to #id */
          }
        }),
      )
      if (active) setChildResolve(() => buildResolver(byModel))
    }
    void load()
    return () => {
      active = false
    }
  }, [child, field.inverse])
  const merged: Resolver = (m, rid) => childResolve(m, rid) ?? resolve?.(m, rid)

  const columns: Column<api.Row>[] = (child?.list.columns ?? [])
    .filter((c) => c.name !== field.inverse) // the back-reference to this parent is implied
    .map((c) => {
      const f = child?.fields.find((cf) => cf.name === c.name)
      const numeric = c.widget === 'monetary' || c.widget === 'integer'
      return {
        header: c.label,
        align: numeric ? ('right' as const) : ('left' as const),
        mono: numeric,
        render: (row: api.Row) => displayValue(row[c.name], c.widget, f, merged),
      }
    })

  return rows.length === 0 || columns.length === 0 ? (
    <div className="t-body rounded-md border border-dashed border-border px-4 py-8 text-center text-muted">No {field.label.toLowerCase()} yet.</div>
  ) : (
    <DataTable columns={columns} rows={rows} rowKey={(r) => r.id} />
  )
}
