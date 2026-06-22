// Access admin page (Studio): grant a runtime DB ACL or add a record rule, live (no restart). DB ACLs
// only WIDEN the compiled-in baseline; record-rule domains are validated server-side against the model.
// Create-only: the server exposes no list endpoint for runtime ACLs/rules yet, so this page configures,
// it does not enumerate. Admin-gated client-side (the endpoints re-check the admin group server-side).

import { useEffect, useRef, useState } from 'react'
import { AlertTriangle, Info, KeyRound, Shield } from 'lucide-react'
import * as api from '../api'
import type { DomainNode } from '../domain'
import { useAuth } from '../auth'
import { Button, Card, Combobox, type ComboOption, PageHeader, useToast } from '../ui'

const LABEL_CLS = 't-caption mb-1.5 block text-muted'
const INPUT_CLS =
  'w-full px-3 rounded-md bg-input text-text border border-input-border placeholder:text-muted shadow-xs ' +
  'transition-[color,box-shadow,border-color] duration-fast ease-out hover:border-muted focus:outline-none ' +
  'focus-visible:border-accent focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-bg ' +
  'focus-visible:shadow-focus'

type Perms = { read: boolean; write: boolean; create: boolean; delete: boolean }
const PERM_KEYS: (keyof Perms)[] = ['read', 'write', 'create', 'delete']

export function Access() {
  const { identity } = useAuth()
  const isAdmin = identity?.groups.includes('admin') ?? false
  const toast = useToast()

  const [models, setModels] = useState<ComboOption[]>([])
  const [groups, setGroups] = useState<ComboOption[]>([])

  const [aclModel, setAclModel] = useState<string | null>(null)
  const [aclGroup, setAclGroup] = useState<string | null>(null)
  const [perms, setPerms] = useState<Perms>({ read: true, write: false, create: false, delete: false })
  const [aclBusy, setAclBusy] = useState(false)

  const [ruleModel, setRuleModel] = useState<string | null>(null)
  const [ruleGroups, setRuleGroups] = useState('')
  const [ruleOps, setRuleOps] = useState('r')
  const [ruleDomain, setRuleDomain] = useState('')
  const [ruleBusy, setRuleBusy] = useState(false)
  const aclSubmitting = useRef(false)
  const ruleSubmitting = useRef(false)

  useEffect(() => {
    void (async () => {
      try {
        const [names, grps] = await Promise.all([api.modelNames(), api.list('res.groups', { limit: 200 })])
        setModels([...names].sort().map((n) => ({ value: n, label: n })))
        setGroups(grps.data.map((g) => ({ value: String(g.name), label: String(g.name) })))
      } catch {
        /* options stay empty; the forms still submit with a typed value */
      }
    })()
  }, [])

  async function submitAcl(): Promise<void> {
    if (!aclModel || !aclGroup || aclSubmitting.current) return
    aclSubmitting.current = true
    setAclBusy(true)
    try {
      await api.setAcl({ model: aclModel, group: aclGroup, ...perms })
      toast.success(`Granted ${aclGroup} access on ${aclModel}`)
    } catch (e: unknown) {
      toast.error(e instanceof api.ApiError ? e.message : 'Could not grant access')
    } finally {
      aclSubmitting.current = false
      setAclBusy(false)
    }
  }

  async function submitRule(): Promise<void> {
    if (!ruleModel || ruleSubmitting.current) return
    let domain: DomainNode
    try {
      const parsed: unknown = JSON.parse(ruleDomain)
      if (typeof parsed !== 'object' || parsed === null) throw new Error('not an object')
      domain = parsed as DomainNode
    } catch {
      toast.error('Domain must be a valid JSON object, e.g. { "field": "state", "op": "=", "value": "sale" }')
      return
    }
    ruleSubmitting.current = true
    setRuleBusy(true)
    try {
      await api.setRule({
        model: ruleModel,
        domain,
        groups: ruleGroups.trim() || undefined,
        ops: ruleOps.trim() || undefined,
      })
      toast.success(`Record rule added on ${ruleModel}`)
      setRuleDomain('')
    } catch (e: unknown) {
      toast.error(e instanceof api.ApiError ? e.message : 'Could not add the rule')
    } finally {
      ruleSubmitting.current = false
      setRuleBusy(false)
    }
  }

  return (
    <div>
      <PageHeader title="Access" subtitle="Runtime ACLs and record rules" />

      <Card className="mb-5 flex items-start gap-3 p-4">
        <Info size={16} className="mt-0.5 shrink-0 text-accent" />
        <p className="t-caption text-muted">
          Changes apply <span className="text-text">live</span>, no restart. A DB ACL only <span className="text-text">widens</span>{' '}
          the compiled-in baseline (it cannot revoke a built-in grant). A record rule restricts which rows a group can
          see; its domain is validated against the model on save.
        </p>
      </Card>

      {!isAdmin && (
        <p className="t-caption mb-5 flex items-center gap-1.5 text-muted">
          <AlertTriangle size={13} /> Managing access requires the admin group.
        </p>
      )}

      <div className="grid gap-5 lg:grid-cols-2">
        {/* ---- ACL ---- */}
        <Card className="p-5">
          <div className="mb-4 flex items-center gap-2">
            <Shield size={16} className="text-accent" />
            <h2 className="t-subtitle font-medium text-text">Grant access</h2>
          </div>
          <div className="space-y-4">
            <label className="block">
              <span className={LABEL_CLS}>Model</span>
              <Combobox value={aclModel} onChange={(v) => setAclModel(v as string | null)} options={models} placeholder="Select a model…" />
            </label>
            <label className="block">
              <span className={LABEL_CLS}>Group</span>
              <Combobox value={aclGroup} onChange={(v) => setAclGroup(v as string | null)} options={groups} placeholder="Select a group…" />
            </label>
            <fieldset>
              <span className={LABEL_CLS}>Permissions</span>
              <div className="flex flex-wrap gap-x-5 gap-y-2">
                {PERM_KEYS.map((k) => (
                  <label key={k} className="inline-flex items-center gap-2 text-sm text-text">
                    <input
                      type="checkbox"
                      className="h-4 w-4 rounded border-input-border bg-input accent-[var(--accent)]"
                      checked={perms[k]}
                      onChange={(e) => setPerms((p) => ({ ...p, [k]: e.target.checked }))}
                    />
                    <span className="capitalize">{k}</span>
                  </label>
                ))}
              </div>
            </fieldset>
            <Button variant="primary" onClick={submitAcl} disabled={!isAdmin || aclBusy || !aclModel || !aclGroup}>
              {aclBusy ? 'Granting…' : 'Grant access'}
            </Button>
          </div>
        </Card>

        {/* ---- Record rule ---- */}
        <Card className="p-5">
          <div className="mb-4 flex items-center gap-2">
            <KeyRound size={16} className="text-accent" />
            <h2 className="t-subtitle font-medium text-text">Add record rule</h2>
          </div>
          <div className="space-y-4">
            <label className="block">
              <span className={LABEL_CLS}>Model</span>
              <Combobox value={ruleModel} onChange={(v) => setRuleModel(v as string | null)} options={models} placeholder="Select a model…" />
            </label>
            <div className="grid grid-cols-2 gap-3">
              <label className="block">
                <span className={LABEL_CLS}>Groups</span>
                <input className={INPUT_CLS} style={{ height: 'var(--control-h)' }} value={ruleGroups} onChange={(e) => setRuleGroups(e.target.value)} placeholder="admin,sales.user — blank = global" />
              </label>
              <label className="block">
                <span className={LABEL_CLS}>Ops</span>
                <input className={INPUT_CLS} style={{ height: 'var(--control-h)' }} value={ruleOps} onChange={(e) => setRuleOps(e.target.value)} placeholder="r,w,c,d" />
              </label>
            </div>
            <label className="block">
              <span className={LABEL_CLS}>Domain (JSON AST)</span>
              <textarea
                className={INPUT_CLS + ' py-2 font-mono text-[12.5px]'}
                rows={4}
                value={ruleDomain}
                onChange={(e) => setRuleDomain(e.target.value)}
                placeholder={'{ "field": "state", "op": "=", "value": "sale" }'}
              />
            </label>
            <Button variant="primary" onClick={submitRule} disabled={!isAdmin || ruleBusy || !ruleModel || !ruleDomain.trim()}>
              {ruleBusy ? 'Adding…' : 'Add rule'}
            </Button>
          </div>
        </Card>
      </div>
    </div>
  )
}
