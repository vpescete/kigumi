// Studio: add a custom field to a model at runtime (admin only). POSTs /_fields, then the caller
// refetches the contract so the new field renders with no recompile. Scalar kinds only — the server
// rejects relations/selection; the kind list here is exactly the server's create-kind vocabulary.

import { useRef, useState } from 'react'
import * as api from '../api'
import { Button, Combobox, Dialog, type ComboOption, useToast } from '../ui'

const KINDS: ComboOption[] = [
  { value: 'text', label: 'Text' },
  { value: 'integer', label: 'Integer' },
  { value: 'float', label: 'Float' },
  { value: 'decimal', label: 'Decimal' },
  { value: 'bool', label: 'Checkbox' },
  { value: 'date', label: 'Date' },
  { value: 'datetime', label: 'Date & time' },
]

// Mirrors the server's is_safe_ident: lowercase start, then letters/digits/underscore. Validated here
// so the obvious mistakes never round-trip to a 400.
const NAME_RE = /^[a-z][a-z0-9_]*$/

const INPUT_CLS =
  'w-full px-3 rounded-md bg-input text-text border border-input-border placeholder:text-muted shadow-xs ' +
  'transition-[color,box-shadow,border-color] duration-fast ease-out hover:border-muted focus:outline-none ' +
  'focus-visible:border-accent focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-bg ' +
  'focus-visible:shadow-focus aria-[invalid=true]:border-danger'

interface AddFieldDialogProps {
  model: string
  existing: string[] // field names already on the model (for a clean duplicate check)
  onClose: () => void
  onAdded: () => void | Promise<void>
}

export function AddFieldDialog({ model, existing, onClose, onAdded }: AddFieldDialogProps) {
  const toast = useToast()
  const [name, setName] = useState('')
  const [label, setLabel] = useState('')
  const [kind, setKind] = useState<api.FieldKind>('text')
  const [busy, setBusy] = useState(false)
  const submitting = useRef(false)

  const duplicate = existing.includes(name)
  const nameValid = NAME_RE.test(name) && !duplicate
  const canSave = nameValid && !busy

  async function submit(): Promise<void> {
    if (!canSave || submitting.current) return // synchronous guard: ignore a double-click within a frame
    submitting.current = true
    setBusy(true)
    try {
      await api.addField(model, { name, label: label.trim() || name, kind })
      toast.success(`Field "${name}" added`)
      await onAdded()
    } catch (e: unknown) {
      toast.error(e instanceof api.ApiError ? e.message : 'Could not add the field')
    } finally {
      submitting.current = false
      setBusy(false)
    }
  }

  return (
    <Dialog
      open
      onClose={onClose}
      title={`Add field to ${model}`}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!canSave}>
            {busy ? 'Adding…' : 'Add field'}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <label className="block">
          <span className="t-caption mb-1.5 block text-muted">Name</span>
          <input
            className={INPUT_CLS}
            style={{ height: 'var(--control-h)' }}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="warranty_months"
            aria-invalid={name.length > 0 && !nameValid}
            autoFocus
          />
          <span className="t-caption mt-1 block text-muted">
            {duplicate
              ? 'A field with this name already exists.'
              : 'Lowercase letters, digits and underscore. This becomes a real column.'}
          </span>
        </label>
        <label className="block">
          <span className="t-caption mb-1.5 block text-muted">Label</span>
          <input
            className={INPUT_CLS}
            style={{ height: 'var(--control-h)' }}
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder={name || 'Warranty (months)'}
          />
        </label>
        <label className="block">
          <span className="t-caption mb-1.5 block text-muted">Type</span>
          <Combobox
            value={kind}
            onChange={(v) => v != null && setKind(v as api.FieldKind)}
            options={KINDS}
            allowClear={false}
          />
        </label>
      </div>
    </Dialog>
  )
}
