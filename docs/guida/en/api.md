# REST API and UI contract

Kigumi is a headless ERP framework: the only integration surface is an HTTP API generated from the catalog of installed models. This page documents the entire API exposed by the `kigumi-server` crate (axum router in `crates/kigumi-server/src/lib.rs`): the JWT authentication flow, the CRUD data routes with their response envelope, the service-method endpoints (variants, pricelist, wizard, discount, report, posting, invoicing, transfer validation), the attachment and chatter endpoints (messages, activities, followers), the shape of the UI contract emitted per model, the OpenAPI document, the error format with its status codes, and the health endpoints. For context on how these pieces fit together see [architettura.md](architettura.md); for starting the server see [installazione.md](installazione.md) and [README.md](README.md); for ACLs, record rules, and multi-company scope see [sicurezza.md](sicurezza.md).

## How the router is mounted

The server exposes two levels depending on how the host (the `kigumi serve` CLI) builds it:

- **Metadata-only router** — `router(models)`: mounts only `GET /openapi.json`, `GET /api/models`, and `GET /api/:name/view`. No database, no authentication.
- **Full router** — `router_with_data(...)` (or `router_with_data_rasterized(...)` to attach a PDF rasterizer): adds the `/auth/*` block, the health endpoints, and all CRUD data routes + service methods + attachments + chatter, with a `DataBackend` that brings in the database, ACLs, record rules, `Authenticator`, and blob store.

The base shared by both is:

```rust
fn base_router() -> Router<AppState> {
    Router::new()
        .route("/openapi.json", get(openapi_handler))
        .route("/api/models", get(models_handler))
        .route("/api/:name/view", get(view_handler))
}
```

The `:name` segment is the **dotted model name** (e.g. `sale.order`, `res.partner`, `product.template`), not the table name. The CLI passes the signing secret as `s.secrets.jwt_secret`, i.e. the env var **`KIGUMI_JWT_SECRET`** (see [configurazione.md](configurazione.md)). The maximum request body limit is 25 MiB (`DefaultBodyLimit::max(MAX_BODY_BYTES)`, with `MAX_BODY_BYTES = 25 * 1024 * 1024`).

## Authentication

Authentication is based on HS256 JWT tokens signed with `KIGUMI_JWT_SECRET`. The issuance and verification logic lives in the `kigumi-auth` crate (`Authenticator`). There are two **kinds** of token, distinguished by the `kind` claim:

- **access token** — short-lived (`ACCESS_TTL = 900` seconds, 15 minutes). It is verified into a `Ctx` (uid, groups, multi-company scope) for every data request. It carries the groups and scope within itself, so every request is verifiable without a round-trip to the database.
- **refresh token** — long-lived (`REFRESH_TTL = 2_592_000` seconds, 30 days), tracked server-side by a `jti` and revocable/rotatable.

The two kinds are separated cryptographically: a refresh token can **never** be used as a bearer to access data, and vice versa (the `kind` claim is checked in `decode_kind`). The algorithm is fixed to HS256 (`Validation::new(Algorithm::HS256)`, which rejects `alg=none` and algorithm confusion) and expiry is validated with no leeway window (`validation.leeway = 0`).

### `POST /auth/login`

Body: `{ "login": "...", "password": "..." }`. Missing credentials yield `400 login and password required`. The password is verified with argon2 against the stored hash; login **always** runs argon2 (against a dummy hash if the user does not exist, via `dummy_hash`), so the timing and body of the `401` are identical for an unknown user and a wrong password (no user enumeration). On success it responds `200` with the token pair:

```json
{
  "access_token": "<jwt>",
  "refresh_token": "<jwt>",
  "token_type": "Bearer",
  "expires_in": 900
}
```

Invalid credentials → `401 invalid credentials`.

```bash
TOKEN=$(curl -s -X POST http://127.0.0.1:8099/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"login":"admin","password":"'"$KIGUMI_ADMIN_PASSWORD"'"}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["access_token"])')
```

> The `admin` user is created on the first `kigumi serve` from `KIGUMI_ADMIN_PASSWORD` (if no admin exists yet); see [installazione.md](installazione.md).

### `Authorization: Bearer` header

Every data route requires the `Authorization: Bearer <access_token>` header. Verification happens in the server's `authenticate` wrapper, which delegates to `kigumi-auth`'s `Authenticator::verify_bearer`: the header must start with the literal `Bearer ` prefix and the token must be a valid access token. Failure produces `401 unauthorized`. The derived `Ctx` is the only identity the server trusts: a client cannot declare a group without a token signed by the secret.

```bash
curl -s http://127.0.0.1:8099/api/res.partner \
  -H "Authorization: Bearer $TOKEN"
```

### `POST /auth/refresh`

Body: `{ "refresh_token": "..." }` (missing → `400 refresh_token required`). The presented token is verified; then the server **atomically claims** it (`claim_refresh`), revoking it: a concurrent replay claims zero rows and is rejected (`401 invalid refresh token`), so no double-spend. On refresh the groups (`user_groups`) and the company scope (`user_scope`) are **re-read** from the database, so group or company reassignments take effect. The response is a **new** `access_token`/`refresh_token` pair (refresh token rotation) with the same shape as login.

### `POST /auth/logout`

Body: `{ "refresh_token": "..." }`. **Always** responds `204 No Content`, without revealing whether the token was valid. If the token is verifiable, its `jti` is revoked server-side (`revoke_refresh`).

### `GET /auth/me`

Returns the identity of the authenticated caller, i.e. the `Ctx` derived from the bearer. Requires a valid access token.

```json
{
  "uid": 1,
  "groups": ["user", "admin", "sales.user"],
  "company_id": 1,
  "allowed_company_ids": [1, 2]
}
```

### SSO via OpenID Connect

When `[oidc]` is configured (see [configurazione.md](configurazione.md)), two routes add browser-based SSO alongside password login. Absent that config, both routes return `404`.

- **`GET /auth/oidc/start`** — begins the login: a `302` to the IdP's authorization endpoint, carrying PKCE, a nonce, and a one-time `state` (recorded server-side with a 10-minute TTL). It also sets a short-lived `HttpOnly` cookie binding the flow to this browser (the login-CSRF defense; the browser carries it automatically). The SPA opens this URL (full-page or popup).
- **`GET /auth/oidc/callback?code=…&state=…`** — the IdP redirects the browser back here. The server consumes the `state` (one shot — it cannot be replayed), exchanges the `code`, verifies the `id_token` (JWKS signature, nonce, `iss`/`aud`/`exp`), and requires a **verified** email. It then resolves the user — an existing account by email, or a just-in-time create with **no groups** and no usable password — mints the session, and `302`s to the configured `post_login_url` with the tokens in the URL **fragment**:

  ```
  https://app.example.com/home#access_token=…&refresh_token=…&token_type=Bearer&expires_in=900
  ```

  The SPA reads `location.hash`, stores the tokens, and clears the fragment. From here the session is identical to a password login (same access/refresh pair). Failures return a status by class (`400` bad/expired state or missing code, `401` token verification failed, `403` unverified email, `502` IdP unreachable) with a generic message; the upstream detail is logged server-side only. The client secret comes from `KIGUMI_OIDC_CLIENT_SECRET`, never a request or the config file.

## API keys (machine credentials)

Long-lived, revocable credentials for machines and agents — the stateful sibling of the refresh
token. A key IMPERSONATES a user: it inherits that user's groups and company scope, optionally
NARROWED to a subset of groups. A key can never exceed the user it belongs to.

Present it exactly like a JWT, in the `Authorization` header — the `kg_` scheme routes it to the
key path:

```
Authorization: Bearer kg_<prefix>_<secret>
```

Managing keys (you must be authenticated with a JWT or another key to manage your own):

### `POST /auth/keys` — mint a key

Body: `name` (required), `scopes` (CSV, a subset of your own groups; omit for all the groups you
hold now), `expires_in` (seconds, optional — omit for no expiry; revocation is the control). The
plain key is returned ONCE and is never recoverable.

```json
// → 201
{ "id": 4, "prefix": "kg_a1b2c3d4...", "key": "kg_a1b2c3d4..._9f8e...", "note": "store this key now — it is not recoverable" }
```

Scopes are FROZEN at mint to the groups you hold at that moment — a narrowed key cannot mint an
un-narrowed one. A scope you do not hold is rejected with `403`.

### `GET /auth/keys` — list your live keys

Returns `{ "data": [ { id, prefix, name, scopes, expires_at, last_used_at, created_at } ] }` — never
the secret or the hash.

### `DELETE /auth/keys/:id` — revoke a key

Soft-delete (`revoked_at`): the key stops authenticating immediately. You can only revoke your own;
an unknown or already-revoked id is `404`.

CLI equivalent for headless automation: `kigumi apikey create <user> --name ... [--scopes ...]
[--expires-days N]`, `kigumi apikey list <user>`, `kigumi apikey revoke <id>`.

Every auth failure — bad secret, unknown/revoked/expired key — is one uniform `401`; the auth path
spends the same work regardless, so timing does not reveal which keys exist.

## Data endpoints (CRUD)

All routes under `/api/:name` require an access token and apply the ACL + record rule + multi-company scope engine in the `kigumi-db` layer (the `*_secured` methods). Authorization is **not** in the server: the handler authenticates, shapes the response, and maps errors.

| Route | Method | What it does |
|---|---|---|
| `/api/models` | GET | JSON array of the names of the served models |
| `/api/:name/view` | GET | UI contract of the model (see below) |
| `/api/:name` | GET | paginated list (envelope `data/total/limit/offset`) |
| `/api/:name` | POST | creates a record, returns `{ "id": <n> }` with `201` |
| `/api/:name/:id` | GET | reads a record |
| `/api/:name/:id` | PATCH | updates a record, returns `{ "updated": <n> }` |
| `/api/:name/:id` | DELETE | deletes a record, returns `{ "deleted": <n> }` |
| `/api/:name/:id/action/:action` | POST | runs a state-transition action |

### `GET /api/models`

JSON array of the (dotted) names of the served models, e.g. `["res.partner", "sale.order", ...]`.

### `GET /api/:name` — paginated list

Responds with a four-field envelope:

```json
{ "data": [ /* record */ ], "total": 123, "limit": 80, "offset": 0 }
```

- `data` — the page of records (the db layer's `ListPage.data`).
- `total` — the total count under the **same** secure filter (not just the page).
- `limit` / `offset` — the values actually applied (echoed back).

#### Pagination, ordering, and filter parameters

| Parameter | Meaning | Notes |
|---|---|---|
| `limit` | page size | default `80` (`DEFAULT_LIMIT`); clamped to `[1, 500]` (`MAX_LIMIT`); non-integer → `400 limit must be an integer` |
| `offset` | offset | default `0`; negative values forced to `0`; non-integer → `400 offset must be an integer` |
| `order` | ordering | comma-separated list; `-` prefix = descending, e.g. `-id` or `name,-amount_total` |
| `domain` | JSON domain AST | escape hatch for arbitrary AND/OR/NOT (see below) |
| `<field>__<op>=<value>` | suffix-operator filter | the default filter, AND-ed conditions |

There are **two filter forms** (decision D5), combinable (AND-ed when both are present):

1. **Suffix operator** `field__op=value` (handled by `split_suffix` + `build_leaf`). A bare `field` with no suffix uses the `eq` operator. Recognized operators:

   | Suffix | Operator |
   |---|---|
   | `eq` | `=` |
   | `ne` | `!=` |
   | `gt` | `>` |
   | `gte` | `>=` |
   | `lt` | `<` |
   | `lte` | `<=` |
   | `like` | `LIKE` |
   | `ilike` | `ILIKE` |
   | `in` | `IN` (value = comma-separated list) |

   The value is coerced to the field type (`coerce`): an unknown suffix, an unknown field, or a non-coercible value (e.g. `'nope'` on an integer field) yield `400`. You cannot filter directly on a `One2many`/`Many2many` field. The `id` field is always filterable (treated as an integer).

2. **Domain AST** `?domain=<json>` (parsed by `Domain::from_json`). JSON-encoded; rejected with `400 invalid domain JSON` if malformed. The form is the same one the server compiles to SQL and the frontend evaluates client-side (see [UI contract](#ui-contract)). Nodes:

   ```json
   { "field": "state", "op": "=", "value": "draft" }
   { "and": [ {"field":"state","op":"=","value":"draft"}, {"field":"amount","op":">=","value":100} ] }
   { "or":  [ /* ... */ ] }
   { "not": { /* ... */ } }
   { "const": true }
   ```

   The `op` tokens allowed in the AST (`op_from_str`) are: `=`, `!=`, `<`, `<=`, `>`, `>=`, `in`, `not in`, `like`, `ilike`, `is null`, `is not null` (for `is null`/`is not null` the `value` is omitted).

Examples:

```bash
# suffix operator + ordering + pagination
curl -s "http://127.0.0.1:8099/api/sale.order?state=draft&amount_total__gte=100&order=-id&limit=20" \
  -H "Authorization: Bearer $TOKEN"

# domain AST
curl -s -G "http://127.0.0.1:8099/api/sale.order" \
  --data-urlencode 'domain={"or":[{"field":"state","op":"=","value":"draft"},{"field":"state","op":"=","value":"sent"}]}' \
  -H "Authorization: Bearer $TOKEN"
```

### `POST /api/:name` — create

Body: a JSON object (a non-object body → `400 body must be a JSON object`). On success `201 Created` with `{ "id": <new id> }`.

```bash
curl -s -X POST http://127.0.0.1:8099/api/res.partner \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"ACME Spa"}'
# 201 → {"id": 42}
```

### `GET /api/:name/:id` — read

Returns the record as a JSON object (`find_one_secured`). `One2many` children are **inlined** as full child objects in the get-one. If the record does not exist or is not permitted by the security engine → `404 not found or not permitted` (non-existence and non-visibility are indistinguishable, so as not to reveal the existence of inaccessible records).

### `PATCH /api/:name/:id` — update

Body: a JSON object with the fields to write. `0` rows updated (record absent or not permitted) → `404 not found or not permitted`; otherwise `200` with `{ "updated": <n> }`.

### `DELETE /api/:name/:id` — delete

`0` rows → `404 not found or not permitted`; otherwise `200` with `{ "deleted": <n> }`.

### `POST /api/:name/:id/action/:action` — state-transition action

Runs a registered action (`run_action`, e.g. confirm a draft order). On success:

```json
{ "ok": true, "action": "confirm" }
```

Errors (unknown action, access denied, invalid transition) follow `write_error` (see [Error format](#error-format-and-status-codes)).

## Service-method endpoints

Cross-record business methods registered by modules on the `register_service!` seam run through the generic dispatch:

`POST /api/:name/:id/service/:service` — body: a JSON object (the service's input), result: the service's JSON output. The service owns ONE transaction (commit on success, rollback on error, including jobs enqueued through it); the optional write gate requires the caller to hold Write on the model, plus any group restriction the registration declares.

A few legacy methods predate the seam and keep dedicated pinned routes. The handler checks the model **pin** (a different name → `400`), authenticates, and shapes the response; authorization and transactional logic live in the `kigumi-db` layer. If the model is not served (module not installed) → `404`.

| Route | Method | Required model | JSON result | Status |
|---|---|---|---|---|
| `/api/:name/:id/generate_variants` | POST | `product.template` | `{ "created": [...], "archived": [...], "kept": [...] }` | `200` |
| `/api/:name/:id/apply_pricelist` | POST | `sale.order` | `{ "priced": <n> }` | `200` |
| `/api/:name/open` | POST | any wizard | the transient record created | `201` |
| `/api/:name/:id/apply_discount` | POST | `sale.order.discount` | `{ "discounted": <n> }` | `200` |
| `/api/:name/:id/report/:report` | GET | any (with a registered report) | HTML, or PDF with `?format=pdf` | `200` |
| `/api/:name/:id/post` | POST | `account.move` | `{ "posted": "<number>" }` | `200` |
| `/api/:name/:id/create_invoice` | POST | `sale.order` | `{ "invoice": <move_id> }` | `200` |
| `/api/:name/:id/validate` | POST | `stock.picking` | `{ "validated": "<number>" }` | `200` |
| `/api/:name/:id/create_delivery` | POST | `sale.order` | `{ "picking": <id> }` | `201` |
| `/api/:name/:id/create_receipt` | POST | `purchase.order` | `{ "picking": <id> }` | `201` |

Detailed notes:

- **`generate_variants`** — materializes the cartesian product of a `product.template`'s attribute lines into `product.product`; the result distinguishes the ids created, archived (a combination no longer selected), and kept.
- **`apply_pricelist`** — re-prices the lines of a `sale.order` from its pricelist; `priced` is the number of re-priced lines.
- **`open` (wizard)** — opens a transient model: it computes its defaults server-side (`default_get`) from the opening context (`active_model` / `active_id` / `active_ids`, all optional in the body), creates the scratchpad row under the caller, and returns it for rendering via the contract. The model must be bound with `register_wizard!` (otherwise `400 not a wizard model`).
- **`report`** — security is read access to the record (`find_one_secured`): being able to read the record is exactly what allows printing it. An unknown report name → `404 unknown report`. Without `?format=pdf` it responds HTML (`text/html`); with `?format=pdf` it rasterizes the same HTML into PDF — but only if a rasterizer is configured, otherwise `501 PDF rendering is not configured`.
- **`post`** — posts a draft `account.move` (balance recheck + per-journal numbering + state → `posted`); returns the assigned number.
- **`validate`** — validates a draft `stock.picking` (`done` moves + stock-level update + numbering, in one transaction).
- **`create_delivery` / `create_receipt`** — create a draft transfer (`Stock → Customers` from a confirmed `sale.order`; `Vendors → Stock` from a confirmed `purchase.order`) and return `201` with the transfer id.

From the web client these record-scoped methods go through a single helper:

```ts
// web/src/api.ts
export async function callEndpoint<T = Record<string, unknown>>(
  model: string,
  id: number,
  path: string,
): Promise<T> {
  return asJson<T>(await request(`/api/${model}/${id}/${path}`, { method: 'POST' }))
}
```

## Module routes: `GET|POST /api/x/:route`

Bespoke module endpoints registered with `register_route!` (webhook receivers, custom searches) are dispatched generically on `/api/x/<name>`, keyed by `(name, method)`. Authenticated routes run under the caller's `Ctx` (plus any group restriction); `auth: false` routes run under the GUEST context (uid −1, no groups — the default-deny ACL blocks every secured call until the body itself verifies the sender, e.g. with the constant-time `RouteInput::verify_hmac_sha256`). Request bodies are capped at 2 MB; a wrong method on an existing name answers `405` with an `Allow` header.

## Live events (SSE): `GET /api/events/stream`

Server-sent events for every committed write, filtered per caller: an event is delivered only if the caller can read the record now (ACLs + record rules re-checked per batch), changed-field names are filtered by field-group visibility, and delete events are suppressed where a read record rule applies. Each event carries an id of the form `txn:id`; reconnect with `Last-Event-ID` for an exact, gap-free resume (the cursor is the pair, so no committed event is skipped or duplicated). Streams are bounded to 15 minutes — clients reconnect and the resume is seamless; access revocation is therefore never stale for longer than one batch.

```
event: message
id: 668129:15
data: {"type":"model.created","model":"workshop.vehicle","record_id":2,"txn":668129,"changes":{},...}
```

Authentication is the same bearer token (`EventSource` cannot send headers — use `fetch` with a readable stream, as `web/src/api.ts` does).

## MCP: the AI surface

Every binary that links its modules can serve the catalog over the Model Context Protocol
(stdio): `kigumi mcp <login>`, or a scaffolded app's `cargo run -p app -- mcp <login>`. Ten tools
are derived from the catalog — `list_models`, `get_model` (the contract), `search_records`
(domain AST, SQL-bounded limit), `get/create/update/delete_record`, `run_action`, `run_service`,
`post_message` — and every one runs under the IMPERSONATED user's `Ctx`: ACLs, record rules and
field visibility enforced by the data layer, validation failures returned as the same structured
envelope as REST. The guardrail is the security engine, not the prompt.

Trust model: impersonation is unauthenticated by design — starting the process already requires
`DATABASE_URL`, so the boundary is operator trust, like every other CLI command. `DATABASE_URL`
is read from the environment first (MCP clients launch servers with env-var config). Example
Claude Code registration:

```sh
claude mcp add myshop --env DATABASE_URL=postgres://localhost/myshop -- \
  /path/to/myshop/target/debug/app mcp mario
```

Runtime custom fields ARE merged (a field added via the API is read/written over MCP too);
runtime DB ACL/rule overlays are not (the compiled-in baseline is the authority here).

### MCP over HTTP (authenticated, network-facing)

`kigumi mcp-http` (or `serve_http` in an embedding) serves the same tools over streamable HTTP at
`/mcp` (default `127.0.0.1:8601`). Unlike stdio, this is a network surface: EVERY tool call
authenticates the request's API key and runs under that key's user, narrowed to the key's scopes —
the same lookup/verify/narrow as the REST server, so an agent never exceeds the key's owner.

```
Authorization: Bearer kg_<prefix>_<secret>
```

The MCP `initialize` handshake needs no key (it is protocol negotiation); every `tools/call`
resolves the key and denies without a live one, so revoking a key stops its agent on the next call.
An MCP client that supports HTTP + a bearer token connects directly; keep `mcp-http` behind a
reverse proxy (TLS, rate limiting) for exposure beyond localhost.

## Ledger reports: `GET /api/reports/:name`

Record-less aggregate queries (a trial balance, a stock valuation) registered with `register_ledger_report!`, returning JSON rows; each report is gated by the Read ACL of the model it declares. Distinct from per-record document reports (`/api/:name/:id/report/:report`).

## Attachments

Attachments are `ir.attachment` rows: the metadata lives in the record, the bytes in a content-addressed blob store (deduplicated by SHA-256 checksum). The routes anchored to the host record are gated by access to the host record: list/download require **read** on the host, upload/delete require **write** on the host.

| Route | Method | Gate | Result |
|---|---|---|---|
| `/api/:name/:id/attachments` | GET | read host | `{ "data": [ /* metadata, no bytes */ ] }` |
| `/api/:name/:id/attachments` | POST | write host | `201` + `{ "id", "name", "mimetype", "file_size", "checksum" }` |
| `/api/attachment/:aid/content` | GET | read of the host record it is attached to | the bytes (stream) |
| `/api/attachment/:aid` | DELETE | write of the host record | `{ "deleted": 1 }` |

The upload sends the **raw bytes** in the body; the filename travels in the `X-Filename` header and the mimetype in the `Content-Type`. An empty upload → `400 empty upload`. On download, only a safe allowlist (`image/png`, `image/jpeg`, `image/gif`, `image/webp`, `application/pdf`) is served `inline`; everything else is forced to `attachment` with `X-Content-Type-Options: nosniff`, so a user-uploaded blob can never execute as a script in the app's origin.

```ts
// web/src/api.ts
export async function uploadAttachment(model: string, id: number, file: File): Promise<number> {
  const res = await request(`/api/${model}/${id}/attachments`, {
    method: 'POST',
    headers: { 'content-type': file.type || 'application/octet-stream', 'x-filename': file.name },
    body: file,
  })
  return (await asJson<{ id: number }>(res)).id
}
```

## Chatter: messages, activities, followers

The mail subsystem adds, to a model that opts in (`mailed = true` in the contract), a thread of messages, activities (to-dos), and followers. All these endpoints are gated by **read** on the host record: you cannot see or write in the thread of a record you cannot read. The host model must have opted into mail (otherwise `400 model '<name>' has no mail thread`).

| Route | Method | What it does |
|---|---|---|
| `/api/:name/:id/messages` | GET | the record's thread, oldest-first; each message carries its tracking diffs |
| `/api/:name/:id/message` | POST | posts a comment or a note |
| `/api/:name/:id/activities` | GET | open to-dos, each with a derived `state` |
| `/api/:name/:id/activity` | POST | schedules a to-do |
| `/api/:name/:id/activities/:aid/done` | POST | marks a to-do as done |
| `/api/:name/:id/followers` | GET | users subscribed to the thread |
| `/api/:name/:id/follow` | POST | subscribes a user (idempotent) |
| `/api/:name/:id/unfollow` | POST | unsubscribes a user (idempotent) |

Details:

- **Messages** — `GET .../messages` responds `{ "data": [...] }`; each message is enriched with a `tracking` array of its field changes (`old_value`/`new_value`). Changes to fields the caller cannot read are **redacted** (field-level security, D6, via `field_accessible`), so the audit trail does not become a second, unprotected read channel.
- **Posting** — `POST .../message` requires a non-empty `body` (otherwise `400 message body is required`). `message_type` accepts `comment` (default) or `note`; any other value → `400 invalid message_type '<other>'`. The author is the authenticated caller (`ctx.uid`), the timestamp is the DB clock.
- **Activities** — the `state` (`overdue` / `today` / `planned`) is **derived** (`activity_state`) from the deadline compared with the DB's current date (ISO strings compare lexicographically). `POST .../activity` requires a non-empty `summary`; `date_deadline` is optional (empty = no deadline); `user_id` is optional and defaults to the caller.
- **Done** — `POST .../activities/:aid/done` sets `active` to false; the activity must belong to that host record, otherwise `404 activity not found on this record`.
- **Follow/unfollow** — both idempotent: re-following an already-followed record is a success (`{ "ok": true, "already": true }`), unsubscribing when not a follower is a success. Only the `admin` group can (un)subscribe a `user_id` other than its own (anti-IDOR, via `ensure_self_or_admin`), otherwise `403 cannot manage another user's subscription`.

## UI contract

`GET /api/:name/view` returns the model's **UI contract**: a frontend-agnostic JSON consumable by any frontend, produced by `to_ui_contract` in `crates/kigumi-schema/src/lib.rs`. It is the same source of truth as the DDL and the OpenAPI, projected for form and table rendering. An unknown model name → `404 unknown model: <name>`.

General shape:

```json
{
  "model": "sale.order",
  "type": "form",
  "mailed": true,
  "fields": [ /* FieldMeta */ ],
  "list": { "columns": [ /* ColumnMeta */ ] },
  "actions": [ /* ActionMeta */ ],
  "reports": [ /* ReportMeta */ ],
  "view": { "groups": [ /* ... */ ], "pages": [ /* ... */ ] }
}
```

### Fields (`fields`)

Each field carries `name`, `label`, a `widget` suggested by the type, `required`, and `readonly`. Computed fields and `related` fields are `readonly: true` (they are server-side resolved mirrors); own fields and delegated fields (`_inherits`) are editable. The `widget` is mapped from the field type:

| Field type | `widget` |
|---|---|
| Text | `char` |
| Html | `html` |
| Image | `image` |
| Integer | `integer` |
| Float | `float` |
| Decimal with currency | `monetary` |
| Decimal without currency | `float` |
| Bool | `boolean` |
| Date | `date` |
| Datetime | `datetime` |
| Selection | `selection` |
| Many2one | `many2one` |
| One2many | `one2many` |
| Many2many | `many2many` |

Additional optional per-field attributes:

- `options` — for `selection`, the `{ "value", "label" }` array of options.
- `relation` — for `many2one`/`one2many`, the target model; `inverse` for `one2many` (the inverse FK field).
- `default` — the declared default value.
- `invisible_when` / `readonly_when` — a **domain AST** (see below) that, when it holds for the record, makes the field invisible/readonly.

The `invisible_when` / `readonly_when` rules are emitted as portable domain ASTs, identical to those accepted by `?domain=` and compiled to SQL by the server. The frontend evaluates them client-side **from the record's data**, never with an eval'd string. Example of an emitted field:

```json
{ "name": "confirm_date", "label": "Confirm Date", "widget": "date", "required": false,
  "readonly": false,
  "invisible_when": { "field": "state", "op": "=", "value": "draft" } }
```

The rules are **validated** at contract construction: a rule that references an unknown or wrongly-typed field makes `to_ui_contract` fail (broken UI rules are rejected, not discovered in production).

### Table (`list.columns`)

The array of columns that a generic table renders (D7): the scalar fields (with a column) plus the on-read computed ones, the related mirrors, and the delegated fields, in declaration order. A `One2many` is not a column. Each column is `{ "name", "label", "widget" }`.

### Actions (`actions`)

The state-transition actions a form can offer as buttons, each with the groups authorized to run it (`groups` empty = everyone). Shape: `{ "name", "groups": [...] }`. The frontend hides the ones the caller's groups do not grant:

```ts
// web/src/api.ts
export function canRun(action: ActionMeta, identity: Identity | null): boolean {
  if (action.groups.length === 0) return true
  if (!identity) return false
  return action.groups.some((g) => identity.groups.includes(g))
}
```

### Reports (`reports`)

The printable documents for a record, each `{ "name", "title" }`. The `name` is the URL segment (`GET /api/:name/:id/report/<name>`), the `title` is the human label also used for the PDF download filename.

### View (`view`)

The form layout declared by the model (`view_for`), or `null` when the model declares no view (the frontend then applies a smart default layout). When present:

```json
{
  "groups": [
    { "title": "Identità", "fields": [ { "name": "name", "full": true }, { "name": "ref", "full": false } ] }
  ],
  "pages": [
    { "title": "Righe", "fields": ["line_ids"] }
  ]
}
```

- `groups` — titled groups of scalar fields (two-column layout in the "sheet"); `title` may be `null` (a leading group with no heading); `full: true` makes the field span both columns (relations, long text, images, primary name).
- `pages` — the notebook pages (tabs) below the sheet, usually a `One2many` relation or secondary details; each page is `{ "title", "fields": [...] }`.

### Translations (i18n)

`GET /api/:name/view` honors the request's **`Accept-Language`** header: when a translation exists for the primary language subtag (e.g. `it` from `it-IT,it;q=0.9`), the served contract's **field labels** and **selection option labels** are swapped for the localized text. Anything without a translation — and every request with no `Accept-Language` — keeps the compile-time English, so this is purely additive. Translation is metadata only; record data is never translated.

Translations are set (admin only) via:

`POST /api/:name/_translation` — body `{ "field", "lang", "text", "value"? }`. An empty or absent `value` translates the field's own label; a non-empty `value` translates that selection option's label. Upserts the `(model, field, value, lang)` row and takes effect on the next contract fetch — no recompile.

```bash
# Italian label for sale.order.state, and for its "draft" option:
curl -X POST -H "Authorization: Bearer $ADMIN" -H 'content-type: application/json' \
  -d '{"field":"state","lang":"it","text":"Stato"}'          .../api/sale.order/_translation
curl -X POST -H "Authorization: Bearer $ADMIN" -H 'content-type: application/json' \
  -d '{"field":"state","value":"draft","lang":"it","text":"Bozza"}' .../api/sale.order/_translation
```

Language negotiation is deliberately minimal: the first tag's primary subtag, exact match, English fallback. Field help text, report/group titles, and a per-user default language are not translated yet.

## OpenAPI document

`GET /openapi.json` returns an **OpenAPI 3.1.0** document generated from the model catalog (`openapi` in `crates/kigumi-schema/src/openapi.rs`). It is pretty-printed, with `info.title = "Kigumi API"` and `info.version = "0.1.0"`. It is meant for generating typed SDKs (TS/Python/Go) with standard tooling (openapi-generator), without hand-written clients.

For each model it emits:

- in `components.schemas`, a model-name-keyed object schema (e.g. `sale.order`) with `id` (`int64`, `readOnly`) and one property per field. Decimals are `string` with `format: decimal` (to preserve NUMERIC precision), dates are `format: date`/`date-time`, `One2many` are arrays of child objects (`$ref` to the child model), `Many2many` are arrays of `int64` ids, computed fields are `readOnly`.
- in `paths`, `GET /api/<table>` (list) and `GET /api/<table>/{id}` (get-one), with `operationId` `list_<table>` and `get_<table>`.

> **Caution — divergence between the spec and the real routes:** the OpenAPI uses the underscored **table name** (`m.table`, e.g. `/api/sale_order`), whereas the data routes actually mounted by the server use the **dotted model name** (`m.name`, e.g. `/api/sale.order`). Moreover, the OpenAPI 3.1 generated in this version documents only `GET` list and `GET` get-one; it does not yet describe the create/update/delete, auth, service-method, attachment, or chatter endpoints. For the complete list refer to this page, not just to the spec.

```bash
curl -s http://127.0.0.1:8099/openapi.json | head -40
```

## Error format and status codes

Errors are returned as a structured JSON envelope, with a status code indicating their class:

```json
{ "error": { "code": "invalid", "message": "hours cannot be negative", "fields": { "hours": ["hours cannot be negative"] } } }
```

`code` is a stable kebab-case class (`bad-input`, `invalid`, `access-denied`, `conflict`, `internal`); `message` is human-readable; `fields` (present on validation errors) maps field names to messages, ready for inline form rendering — `@api.constrains` violations carry the rule's declared fields, not-null rejections carry the missing column. The internal detail (schema, SQL, Postgres error text) **never** reaches the client: unmapped errors are logged server-side and returned as an opaque `500` envelope.

| Status | When | Example body |
|---|---|---|
| `400 Bad Request` | invalid input: non-object body, invalid filter field/operator/domain, non-coercible value, wrong model for a pinned method, invalid `message_type`, missing message body/summary, non-integer `user_id`/`limit`/`offset` | `body must be a JSON object`, `invalid domain JSON: ...` |
| `401 Unauthorized` | no token or invalid/expired token; wrong login credentials; invalid/already-spent refresh token | `unauthorized`, `invalid credentials`, `invalid refresh token` |
| `403 Forbidden` | access denied by the ACL / record rule; attempt to manage another user's subscription without being admin | `access denied`, `cannot manage another user's subscription` |
| `404 Not Found` | unknown model; record absent or not permitted; report/attachment/activity not found | `unknown model: <name>`, `not found or not permitted`, `unknown report` |
| `409 Conflict` | constraint violation (e.g. unique) on a write | the conflict text |

Additional non-error or service statuses: `201 Created` (create, upload, open wizard, create_delivery/receipt, follow), `204 No Content` (logout), `501 Not Implemented` (PDF report with no rasterizer configured), `503 Service Unavailable` (readiness, see below).

The web client models this convention with an `ApiError` carrying the `status` and the text body, and retries **exactly once** transparently on a `401` by refreshing the token:

```ts
// web/src/api.ts
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
```

## Health endpoints

Two endpoints for container probes, mounted only by the full router:

| Route | Method | What it does | Response |
|---|---|---|---|
| `/health` | GET | **liveness**: the process is up. No DB access (fast probe). | `200` `{"status":"ok"}` |
| `/ready` | GET | **readiness**: the process can serve traffic (database reachable via `db.ping()`). | `200` `{"status":"ready"}` or `503` `{"status":"not_ready"}` |

```bash
curl -s http://127.0.0.1:8099/health   # {"status":"ok"}
curl -s http://127.0.0.1:8099/ready    # {"status":"ready"} or 503
```

## References

- Router and handlers, envelope, status codes, `write_error`: `crates/kigumi-server/src/lib.rs`
- UI contract and OpenAPI: `crates/kigumi-schema/src/lib.rs`, `crates/kigumi-schema/src/openapi.rs`
- Token issuance/verification: `crates/kigumi-auth/src/lib.rs`
- Domain AST (filters, `invisible_when`/`readonly_when`): `crates/kigumi-core/src/domain.rs`
- Form view (groups and pages): `crates/kigumi-core/src/view.rs`
- TypeScript client for the same API: `web/src/api.ts`

See also [moduli.md](moduli.md) and [moduli-custom.md](moduli-custom.md) for how models, actions, reports, views, and wizards are declared and registered at compile time.
