// Live API client for the Meshble server. The frontend is data-driven: it reads each model's
// contract from /api/:name/view (fields + list columns + actions, per decision D7) and performs
// CRUD/actions against /api/:name. In dev, Vite proxies /api and /auth to `meshble serve` (:8099),
// so paths stay same-origin. Tokens live in localStorage; a 401 triggers one transparent refresh.

import type { DomainNode } from './domain'

const TOKENS_KEY = 'meshble.tokens'

type Tokens = { access: string; refresh: string }

function loadTokens(): Tokens | null {
  try {
    const raw = localStorage.getItem(TOKENS_KEY)
    return raw ? (JSON.parse(raw) as Tokens) : null
  } catch {
    return null
  }
}

function saveTokens(t: Tokens | null): void {
  if (t) localStorage.setItem(TOKENS_KEY, JSON.stringify(t))
  else localStorage.removeItem(TOKENS_KEY)
}

export class ApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

// ---- Contract types (the shape the server emits at /api/:name/view) ----

// The runtime field types the server can CREATE via /_fields. Scalars, plus `many2one` (a link to a
// record — needs a `relation` target model). `bool` (not boolean), `decimal` distinct from `float`.
export type FieldKind = 'text' | 'integer' | 'float' | 'decimal' | 'bool' | 'date' | 'datetime' | 'many2one'

// The widgets a view override may re-assign (the renderer's vocabulary). `widget` on FieldMeta stays a
// bare string — this union only types the optional override input.
export type FieldWidget =
  | 'char'
  | 'text'
  | 'html'
  | 'integer'
  | 'float'
  | 'monetary'
  | 'boolean'
  | 'selection'
  | 'many2one'
  | 'many2many'
  | 'one2many'
  | 'date'
  | 'datetime'
  | 'image'

export type FieldMeta = {
  name: string
  label: string
  widget: string
  required: boolean
  readonly: boolean
  options?: { value: string; label: string }[]
  default?: string
  relation?: string // target model for many2one/one2many
  inverse?: string // inverse FK field for one2many
  invisible_when?: unknown
  readonly_when?: unknown
}

export type ColumnMeta = { name: string; label: string; widget: string }
export type ActionMeta = { name: string; groups: string[] }
export type ReportMeta = { name: string; title: string }

// Form layout (server-declared): grouped scalar fields in the sheet + notebook pages (tabs) for
// One2many relations / secondary details. `view` is null when the model declares no layout.
export type ViewSlot = { name: string; full: boolean }
export type ViewGroup = { title: string | null; fields: ViewSlot[] }
export type ViewPage = { title: string; fields: string[] }
export type FormViewMeta = { groups: ViewGroup[]; pages: ViewPage[] }

export type Contract = {
  model: string
  type: string
  mailed?: boolean // model has a chatter thread (messages/activities/followers)
  fields: FieldMeta[]
  list: { columns: ColumnMeta[] }
  actions: ActionMeta[]
  reports?: ReportMeta[] // printable documents (GET .../report/<name>); read as `contract.reports ?? []`
  view?: FormViewMeta | null // declared form layout, or null -> the frontend applies a smart default
}

// ---- Chatter (mail subsystem) ----

export type TrackingChange = {
  field: string
  old_value: string | null
  new_value: string | null
}
export type Message = {
  id: number
  res_model: string
  res_id: number
  author_id: number | null
  message_type: 'comment' | 'note' | 'notification'
  body: string | null
  date: string | null
  tracking: TrackingChange[]
}
export type ActivityState = 'overdue' | 'today' | 'planned'
export type Activity = {
  id: number
  summary: string
  date_deadline: string | null
  user_id: number | null
  state: ActivityState
}
export type Follower = { id: number; user_id: number }

export type Row = { id: number } & Record<string, unknown>
export type Page = { data: Row[]; total: number; limit: number; offset: number }

export type Identity = {
  uid: number
  groups: string[]
  company_id: number | null
  allowed_company_ids: number[]
}

export type ListQuery = {
  limit?: number
  offset?: number
  order?: string // comma list of columns; prefix "-" for descending, e.g. "-id" or "name,-amount_total"
  domain?: unknown // portable domain AST; JSON-encoded into ?domain=
}

// ---- Transport: bearer attach + single transparent refresh on 401 ----

async function request(path: string, init?: RequestInit, allowRetry = true): Promise<Response> {
  const tokens = loadTokens()
  const headers = new Headers(init?.headers)
  if (tokens) headers.set('authorization', `Bearer ${tokens.access}`)
  const res = await fetch(path, { ...init, headers })
  if (res.status === 401 && allowRetry && tokens && (await tryRefresh())) {
    return request(path, init, false)
  }
  return res
}

async function asJson<T>(res: Response): Promise<T> {
  if (!res.ok) throw new ApiError(res.status, (await res.text()) || res.statusText)
  return (await res.json()) as T
}

/// Throws an ApiError (the response body, or `msg`) on a non-2xx response; for endpoints with no body.
async function expectOk(res: Response, msg: string): Promise<void> {
  if (!res.ok) throw new ApiError(res.status, (await res.text()) || msg)
}

async function tryRefresh(): Promise<boolean> {
  const tokens = loadTokens()
  if (!tokens) return false
  const res = await fetch('/auth/refresh', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ refresh_token: tokens.refresh }),
  })
  if (!res.ok) {
    saveTokens(null)
    return false
  }
  const t = (await res.json()) as { access_token: string; refresh_token: string }
  saveTokens({ access: t.access_token, refresh: t.refresh_token })
  return true
}

// ---- Auth ----

export async function login(loginName: string, password: string): Promise<void> {
  const res = await fetch('/auth/login', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ login: loginName, password }),
  })
  if (!res.ok) throw new ApiError(res.status, 'invalid credentials')
  const t = (await res.json()) as { access_token: string; refresh_token: string }
  saveTokens({ access: t.access_token, refresh: t.refresh_token })
}

export async function logout(): Promise<void> {
  const tokens = loadTokens()
  if (tokens) {
    await fetch('/auth/logout', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ refresh_token: tokens.refresh }),
    }).catch(() => undefined)
  }
  saveTokens(null)
}

export function isAuthenticated(): boolean {
  return loadTokens() !== null
}

export async function me(): Promise<Identity> {
  return asJson<Identity>(await request('/auth/me'))
}

// ---- Catalog + data ----

export async function modelNames(): Promise<string[]> {
  return asJson<string[]>(await request('/api/models'))
}

// ---- Modules (install / uninstall) ----

export type ModuleInfo = {
  name: string
  version: string
  summary: string
  framework: string
  depends: { name: string; req: string }[]
  installed: boolean
}

export async function modules(): Promise<ModuleInfo[]> {
  return asJson<ModuleInfo[]>(await request('/api/modules'))
}

export async function installModule(name: string): Promise<{ installed: string[]; needs_restart: boolean }> {
  return asJson(await request(`/api/modules/${name}/install`, { method: 'POST' }))
}

export async function uninstallModule(name: string): Promise<{ uninstalled: string; needs_restart: boolean }> {
  return asJson(await request(`/api/modules/${name}/uninstall`, { method: 'POST' }))
}

export async function contract(model: string): Promise<Contract> {
  return asJson<Contract>(await request(`/api/${model}/view`))
}

export async function list(model: string, query: ListQuery = {}): Promise<Page> {
  const q = new URLSearchParams()
  if (query.limit != null) q.set('limit', String(query.limit))
  if (query.offset != null) q.set('offset', String(query.offset))
  if (query.order) q.set('order', query.order)
  if (query.domain !== undefined) q.set('domain', JSON.stringify(query.domain))
  const qs = q.toString()
  return asJson<Page>(await request(`/api/${model}${qs ? `?${qs}` : ''}`))
}

export async function getOne(model: string, id: number): Promise<Row | null> {
  const res = await request(`/api/${model}/${id}`)
  if (res.status === 404) return null
  return asJson<Row>(res)
}

export async function create(model: string, values: Record<string, unknown>): Promise<number> {
  const res = await request(`/api/${model}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(values),
  })
  const { id } = await asJson<{ id: number }>(res)
  return id
}

export async function update(
  model: string,
  id: number,
  values: Record<string, unknown>,
): Promise<void> {
  await asJson<{ updated: number }>(
    await request(`/api/${model}/${id}`, {
      method: 'PATCH',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(values),
    }),
  )
}

export async function remove(model: string, id: number): Promise<void> {
  const res = await request(`/api/${model}/${id}`, { method: 'DELETE' })
  if (!res.ok && res.status !== 404) throw new ApiError(res.status, await res.text())
}

export async function runAction(model: string, id: number, action: string): Promise<void> {
  const res = await request(`/api/${model}/${id}/action/${action}`, { method: 'POST' })
  if (!res.ok) throw new ApiError(res.status, (await res.text()) || 'action failed')
}

/// Whether `identity` may run `action` (empty groups = everyone).
export function canRun(action: ActionMeta, identity: Identity | null): boolean {
  if (action.groups.length === 0) return true
  if (!identity) return false
  return action.groups.some((g) => identity.groups.includes(g))
}

// ---- Reports ----

/** The HTML body of a record's report (text/html), fetched with the bearer (a bare <a> cannot). */
export async function reportHtml(model: string, id: number, report: string): Promise<string> {
  const res = await request(`/api/${model}/${id}/report/${report}`)
  if (!res.ok) throw new ApiError(res.status, (await res.text()) || 'report failed')
  return res.text()
}

/** The PDF blob of a record's report. A 501 means no rasterizer is configured (HTML stays available). */
export async function reportPdf(model: string, id: number, report: string): Promise<Blob> {
  const res = await request(`/api/${model}/${id}/report/${report}?format=pdf`)
  if (res.status === 501) throw new ApiError(501, 'PDF rendering is not configured')
  if (!res.ok) throw new ApiError(res.status, (await res.text()) || 'pdf failed')
  return res.blob()
}

// ---- Wizards (transient models) + record-scoped service methods ----

export type WizardOpenCtx = { active_model?: string; active_id?: number; active_ids?: number[] }

/** Opens a transient wizard seeded from the host record's context; returns the new transient row. */
export async function openWizard(model: string, ctx: WizardOpenCtx): Promise<Row> {
  return asJson<Row>(
    await request(`/api/${model}/open`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(ctx),
    }),
  )
}

/** Calls a record-scoped service endpoint (POST /api/:model/:id/:path) and returns its JSON body —
 * covers create_invoice, apply_pricelist, post, generate_variants, apply_discount, … */
export async function callEndpoint<T = Record<string, unknown>>(
  model: string,
  id: number,
  path: string,
): Promise<T> {
  return asJson<T>(await request(`/api/${model}/${id}/${path}`, { method: 'POST' }))
}

/** Registers a (full or partial) payment against a posted invoice; returns the payment move id. */
export async function registerPayment(moveId: number, amount: string, journalId: number): Promise<number> {
  const res = await request(`/api/account.move/${moveId}/register_payment`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ amount, journal_id: journalId }),
  })
  const { payment } = await asJson<{ payment: number }>(res)
  return payment
}

/** Product onchange for an order/invoice line: returns the values to default (name, price_unit,
 * product_uom_qty, uom_id) when a product is picked, so the client fills the line without typing. */
export async function onchangeProduct(lineModel: string, productId: number): Promise<Record<string, unknown>> {
  const res = await request(`/api/${lineModel}/_onchange`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ product_id: productId }),
  })
  const { values } = await asJson<{ values: Record<string, unknown> }>(res)
  return values
}

// ---- Attachments (image / file fields) ----

/** Uploads a file as an attachment on a record; returns the new attachment id (used as an Image FK). */
export async function uploadAttachment(model: string, id: number, file: File): Promise<number> {
  const res = await request(`/api/${model}/${id}/attachments`, {
    method: 'POST',
    headers: { 'content-type': file.type || 'application/octet-stream', 'x-filename': file.name },
    body: file,
  })
  return (await asJson<{ id: number }>(res)).id
}

/** Fetches an attachment's bytes with the bearer (a bare <img src> cannot carry it). */
export async function attachmentBlob(attachmentId: number): Promise<Blob> {
  const res = await request(`/api/attachment/${attachmentId}/content`)
  if (!res.ok) throw new ApiError(res.status, 'could not load the attachment')
  return res.blob()
}

// ---- Chatter endpoints (gated on read access to the host record) ----

export async function messages(model: string, id: number): Promise<Message[]> {
  const { data } = await asJson<{ data: Message[] }>(await request(`/api/${model}/${id}/messages`))
  return data
}

export async function postMessage(
  model: string,
  id: number,
  body: string,
  messageType: 'comment' | 'note' = 'comment',
): Promise<void> {
  const res = await request(`/api/${model}/${id}/message`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ body, message_type: messageType }),
  })
  await expectOk(res, 'post failed')
}

export async function activities(model: string, id: number): Promise<Activity[]> {
  const { data } = await asJson<{ data: Activity[] }>(await request(`/api/${model}/${id}/activities`))
  return data
}

export async function scheduleActivity(
  model: string,
  id: number,
  summary: string,
  dateDeadline?: string,
): Promise<void> {
  const res = await request(`/api/${model}/${id}/activity`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ summary, date_deadline: dateDeadline ?? '' }),
  })
  await expectOk(res, 'schedule failed')
}

export async function activityDone(model: string, id: number, activityId: number): Promise<void> {
  const res = await request(`/api/${model}/${id}/activities/${activityId}/done`, { method: 'POST' })
  await expectOk(res, 'done failed')
}

export async function followers(model: string, id: number): Promise<Follower[]> {
  const { data } = await asJson<{ data: Follower[] }>(await request(`/api/${model}/${id}/followers`))
  return data
}

export async function setFollow(model: string, id: number, follow: boolean): Promise<void> {
  const res = await request(`/api/${model}/${id}/${follow ? 'follow' : 'unfollow'}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: '{}',
  })
  await expectOk(res, 'follow failed')
}

// ---- Studio: runtime declarative changes (admin only; the server re-checks the admin group) ----
// Each is a thin POST that echoes a tiny ack; callers refresh by re-running `contract(model)`.

/** Adds a custom field to a model at runtime: a real column + a contract entry, no recompile.
 * `relation` is the target model, required when `kind` is 'many2one'. */
export async function addField(
  model: string,
  field: { name: string; label: string; kind: FieldKind; relation?: string },
): Promise<void> {
  await expectOk(
    await request(`/api/${model}/_fields`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(field),
    }),
    'add field failed',
  )
}

/** Overrides a field's UI on a model (relabel / hide / lock / re-widget, or a conditional domain).
 * Omit a key to leave it; the `*_when` domains are validated against the model server-side. */
export async function setView(
  model: string,
  override: {
    field: string
    label?: string
    widget?: FieldWidget
    invisible?: boolean
    readonly?: boolean
    // null clears a stored condition; undefined (omitted) leaves it unchanged.
    invisible_when?: DomainNode | null
    readonly_when?: DomainNode | null
  },
): Promise<void> {
  await expectOk(
    await request(`/api/${model}/_view`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(override),
    }),
    'set view failed',
  )
}

export type ViewOverrideRow = {
  field: string
  label: string | null
  widget: string | null
  invisible: boolean
  readonly: boolean
  invisible_when: DomainNode | null
  readonly_when: DomainNode | null
}

/** The view overrides configured on a model (admin only) — needed to surface hidden fields, which the
 * contract drops, so a Studio UI can offer to show them again. */
export async function viewOverrides(model: string): Promise<ViewOverrideRow[]> {
  return asJson<ViewOverrideRow[]>(await request(`/api/${model}/_view`))
}

/** Grants (or updates) a runtime DB ACL for (model, group). DB ACLs only widen the static baseline. */
export async function setAcl(acl: {
  model: string
  group: string
  read: boolean
  write: boolean
  create: boolean
  delete: boolean
}): Promise<void> {
  await expectOk(
    await request('/api/_acl', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(acl),
    }),
    'set acl failed',
  )
}

/** Adds a runtime record rule. `groups`/`ops` are CSV strings (blank groups = global); `domain` is the AST. */
export async function setRule(rule: {
  model: string
  domain: DomainNode
  groups?: string
  ops?: string
}): Promise<void> {
  await expectOk(
    await request('/api/_rule', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(rule),
    }),
    'set rule failed',
  )
}
