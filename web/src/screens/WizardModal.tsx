// A generic wizard flow: open a transient model seeded from the host record (POST /open), render its
// fields with the shared ContractFields renderer in a dialog, then save the edits and run the apply
// endpoint. Adding a future wizard is a registry entry — this component is reused as-is.

import { useEffect, useMemo, useState } from 'react'
import * as api from '../api'
import { Button, Dialog, useToast } from '../ui'
import { ContractFields, isWritable, useRelOptions } from './ContractFields'
import type { WizardSpec } from '../registries/wizards'

export function WizardModal({
  spec,
  hostModel,
  hostId,
  onClose,
  onApplied,
}: {
  spec: WizardSpec
  hostModel: string
  hostId: number
  onClose: () => void
  onApplied: () => void
}) {
  const toast = useToast()
  const [contract, setContract] = useState<api.Contract | null>(null)
  const [transient, setTransient] = useState<api.Row | null>(null)
  const [values, setValues] = useState<Record<string, unknown>>({})
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  // Show only the wizard's business fields (the seeded context + timestamps stay hidden).
  const shown = useMemo<api.Contract | null>(
    () => (contract ? { ...contract, fields: contract.fields.filter((f) => spec.fields.includes(f.name)) } : null),
    [contract, spec.fields],
  )
  const relOptions = useRelOptions(shown)

  useEffect(() => {
    let cancelled = false
    void Promise.all([
      api.contract(spec.wizardModel),
      api.openWizard(spec.wizardModel, { active_model: hostModel, active_id: hostId }),
    ])
      .then(([c, t]) => {
        if (cancelled) return
        setContract(c)
        setTransient(t)
        setValues({ ...t })
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : 'Could not open the wizard')
      })
    return () => {
      cancelled = true
    }
  }, [spec.wizardModel, hostModel, hostId])

  async function apply(): Promise<void> {
    if (!shown || !transient) return
    setBusy(true)
    try {
      const payload: Record<string, unknown> = {}
      for (const f of shown.fields) {
        if (isWritable(f, values) && values[f.name] !== undefined) payload[f.name] = values[f.name]
      }
      await api.update(spec.wizardModel, transient.id, payload)
      const result = await api.callEndpoint(spec.wizardModel, transient.id, spec.applyPath)
      toast.success(spec.resultToast(result))
      onApplied()
      onClose()
    } catch (e: unknown) {
      toast.error(e instanceof api.ApiError ? e.message : `${spec.label} failed`)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog
      open
      onClose={onClose}
      title={spec.label}
      size="md"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void apply()} disabled={busy || !shown}>
            {busy ? 'Applying…' : spec.label}
          </Button>
        </>
      }
    >
      {error ? (
        <div className="t-body text-danger">{error}</div>
      ) : !shown ? (
        <div className="t-body py-8 text-center text-muted">Opening…</div>
      ) : (
        <ContractFields contract={shown} values={values} relOptions={relOptions} onChange={(name, v) => setValues((p) => ({ ...p, [name]: v }))} />
      )}
    </Dialog>
  )
}
