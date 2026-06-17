// Live API client for the Meshble server. The frontend is data-driven: it reads each model's
// contract from /api/:name/view (fields + list columns + actions, per decision D7) and performs
// CRUD/actions against /api/:name. In dev, Vite proxies /api and /auth to `meshble serve` (:8099),
// so paths stay same-origin. Tokens live in localStorage; a 401 triggers one transparent refresh.

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

export type Contract = {
  model: string
  type: string
  fields: FieldMeta[]
  list: { columns: ColumnMeta[] }
  actions: ActionMeta[]
}

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
