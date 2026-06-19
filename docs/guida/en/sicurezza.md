# Security model

Meshble is headless and schema-driven: every data access passes through a single boundary — the `*_secured` methods of the [`meshble-db`](architettura.md) crate — where authentication produces a trusted identity (`Ctx`) and authorization is enforced in a single place, on every read and write. This page describes authentication (HS256 JWT tokens, revocation, secret rotation), the authorization layers (ACLs, record rules, multi-company scope, field-level groups, sudo / elevated effects), the domain AST, and input validation at the write boundary, with practical guidance for anyone writing a module. For the overview see [README.md](README.md), for the architecture [architettura.md](architettura.md), for the REST routes [api.md](api.md), for the configuration [configurazione.md](configurazione.md).

## Authentication

Authentication lives in the `meshble-auth` crate (`crates/meshble-auth/src/lib.rs`). Tokens are **HS256-signed JWTs** with a shared secret; cryptography is delegated to the `jsonwebtoken` crate, and passwords use `argon2`.

### Typed tokens: access and refresh

There are two token types, distinguished by the `kind` claim:

| Token | `kind` | Effective TTL | Contents (claims) | What it's for |
|-------|--------|---------------|-------------------|--------------|
| **access** | `"access"` | `ACCESS_TTL` = `900` s (15 min) | `sub` (uid), `kind`, `groups`, `company`, `companies`, `exp` | Bearer for every data request: verified into a trusted `Ctx` |
| **refresh** | `"refresh"` | `REFRESH_TTL` = `2_592_000` s (30 days) | `sub` (uid), `kind`, `jti`, `exp` | Proves identity to issue a new access token; never used as a bearer |

The effective TTLs are server constants (`crates/meshble-server/src/lib.rs`):

```rust
const ACCESS_TTL: u64 = 900; // 15 minutes
const REFRESH_TTL: u64 = 2_592_000; // 30 days
```

The `meshble.toml` file exposes an `[auth]` section with the same default values (`access_ttl = 900`, `refresh_ttl = 2592000`), and `meshble-config` defines the same defaults; in v1 token issuance uses the server constants directly (`issue_token_pair`), so these are the effective values at runtime.

The separation between the two types is an explicit guarantee: the `kind` claim is verified on every decode (`decode_kind`), so **a refresh token can never be used as a bearer to access data**, and vice versa. This prevents a long-lived refresh token from acting as an all-powerful bearer.

```rust
fn decode_kind(&self, token: &str, kind: &str) -> Result<Claims, AuthError> {
    // Pin HS256 (rejects alg=none/confusion) and validate exp with no grace window.
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(self.secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AuthError::Invalid)?;
    if data.claims.kind != kind {
        return Err(AuthError::Invalid);
    }
    Ok(data.claims)
}
```

The algorithm is pinned to `HS256` at verification time: this rejects tokens with `alg=none` and algorithm-confusion attacks. Expiration is validated with no grace window (`leeway = 0`).

### The trusted `Ctx` derived from the Bearer

A data request presents an `Authorization: Bearer <token>` header. The server verifies it into a `Ctx` — the trusted identity that flows through the entire security engine (`authenticate` in `crates/meshble-server/src/lib.rs`):

```rust
/// Verifies the request's bearer token into a trusted `Ctx`, or a 401 response. This is real
/// authentication: a client cannot claim a group without a token signed by the server secret.
fn authenticate(backend: &DataBackend, headers: &HeaderMap) -> Result<Ctx, Response> {
    let header = headers.get("authorization").and_then(|v| v.to_str().ok());
    backend
        .auth
        .verify_bearer(header)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "unauthorized").into_response())
}
```

`verify_bearer` extracts the `Bearer ` prefix, verifies the token as **access** (`verify_access`), and builds the `Ctx`. Because the `groups` and the company scope travel signed inside the token, **a client cannot claim a group without a token signed by the server secret**: there's no extra round-trip to the database on every request.

The `Ctx` (defined in `crates/meshble-core/src/security.rs`) carries:

```rust
pub struct Ctx {
    pub uid: i64,
    pub groups: Vec<String>,
    su: bool,                              // private: nobody can forge an elevated context
    pub company_id: Option<i64>,           // active company
    pub allowed_company_ids: Vec<i64>,     // accessible companies (the multi-company scope)
}
```

The `su` flag is **private**: external code cannot construct a `Ctx { su: true, .. }` with a struct literal. The only way to elevate a context is the greppable `Ctx::sudo()` method.

The `GET /auth/me` endpoint returns exactly the fields of the `Ctx` derived from the token (`uid`, `groups`, `company_id`, `allowed_company_ids`).

### Token lifecycle: login, refresh, logout

| Route | Body | Effect |
|-------|-------|---------|
| `POST /auth/login` | `{ "login", "password" }` | Verifies the credentials (argon2) and issues the access+refresh pair |
| `POST /auth/refresh` | `{ "refresh_token" }` | Claims (revokes) the presented refresh and issues a fresh pair (rotation) |
| `POST /auth/logout` | `{ "refresh_token" }` | Revokes the presented refresh token (always responds `204`) |
| `GET /auth/me` | — | Returns the identity (`Ctx`) of the presented bearer |

Login **always** runs argon2 — against a dummy hash (`dummy_hash`) if the user is unknown — so that response timing and the 401 body are identical for a nonexistent user and a wrong password (no user enumeration via timing). On refresh, `groups` and the company scope are **re-read from the database** (`user_groups` and `user_scope`), so that group or company reassignments take effect without re-login.

### Token revocation (jti)

Refresh tokens are **stateful**: each is recorded by `jti` in the `meshble_refresh` table (`crates/meshble-db/src/auth_store.rs`), so it can be revoked (logout) and rotated (each refresh invalidates the previous one). A stolen but revoked refresh token is rejected.

Rotation on refresh is atomic and replay-proof: `claim_refresh` checks and revokes in **a single** SQL statement, so two concurrent claims of the same token cannot both succeed (the loser updates zero rows → rejected), preventing double-spend.

```rust
/// Atomically claims (revokes) an active refresh token, returning its user id. The check and
/// the revoke happen in ONE statement, so two concurrent claims of the same token cannot both
/// succeed: the loser's UPDATE affects zero rows → `None`. This prevents refresh double-spend.
pub async fn claim_refresh(&self, jti: &str) -> Result<Option<i64>, DbError> {
    let row = sqlx::query(
        "UPDATE meshble_refresh SET revoked = true \
         WHERE jti = $1 AND NOT revoked AND expires_at > now() RETURNING user_id",
    )
    .bind(jti)
    .fetch_optional(&self.pool)
    .await?;
    Ok(row.map(|r| r.get("user_id")))
}
```

Access tokens, by contrast, are **stateless and short-lived**: they are not tracked. Immediate revocation applies to the refresh; an access token stays valid until it expires (15 minutes). It's precisely this short-access / long-revocable-refresh pairing that makes the separation worthwhile.

### Secret rotation: `MESHBLE_JWT_SECRET` and `MESHBLE_JWT_SECRET_OLD`

Secrets are read **only from the environment**, never from `meshble.toml`, and the presence of the mandatory ones is checked at boot (fail-fast). Excerpt from `.env.example`:

```bash
# REQUIRED — HS256 signing secret for access/refresh tokens.
MESHBLE_JWT_SECRET=CHANGE_ME_long_random_value
# OPTIONAL — previous JWT secret, still accepted on verify during a rotation window.
# MESHBLE_JWT_SECRET_OLD=
```

`Secrets::from_env` (`crates/meshble-config/src/secrets.rs`) loads `MESHBLE_JWT_SECRET` as mandatory and `MESHBLE_JWT_SECRET_OLD` as optional:

```rust
jwt_secret: req("MESHBLE_JWT_SECRET")?,
jwt_secret_old: opt("MESHBLE_JWT_SECRET_OLD"),
```

The intended rotation model is: set `MESHBLE_JWT_SECRET_OLD` to the previous secret when you introduce a new `MESHBLE_JWT_SECRET`; during the rotation window, tokens signed with the old secret remain accepted at verification, while new tokens are signed with the new one. Both secrets appear (masked) in the server's configuration summary.

> **Implementation note (v1)**: `MESHBLE_JWT_SECRET_OLD` is already read and propagated into `Secrets.jwt_secret_old` (and shown masked in the configuration summary), but `Authenticator::new(...)` accepts **a single secret** (`pub struct Authenticator { secret: String }`), and the `meshble serve` command wires only `s.secrets.jwt_secret`. Verification with the old secret is therefore not yet active in the runtime path: wiring the second secret into `Authenticator` is the step that completes rotation without invalidating in-flight tokens. See [Uncertainties](#incertezze-e-note) and the `Authenticator` in `crates/meshble-auth/src/lib.rs`.

## Authorization: a single enforcement point

Authorization is not scattered across controllers: it lives in the `*_secured` methods of `meshble-db` (`crates/meshble-db/src/lib.rs`), traversed by **every** protected read and write. The checks that come into play are, in all cases:

- the model's **ACL** for the operation (`check_access`) — default-deny;
- **field-level groups** on the touched fields (`field_accessible`, via `strip_unreadable` / `check_writable_fields` / constraints on filter and order-by);
- the model's **record rules** for the operation, compiled into the `WHERE` (`record_rule_domain`);
- **multi-company scope** (`apply_company_scope` on write, `company_filter` / `company_clause` on read) — default-deny on shared rows;
- on write, **input validation** (`validate_write_values`: required, types, rejection of computed fields).

The exact order depends on the direction of the operation:

- **On read** (`read_secured` / building the search domain): first the `Read` ACL, then the record-rule domain and the multi-company one are **AND-ed** into the `WHERE`; a client filter or order-by that references an unreadable field is rejected; after the fetch, `strip_unreadable` removes unreadable fields from the rows.
- **On write** (`insert_secured` / `update_secured` / `delete_secured`): first the ACL (`Create`/`Write`/`Delete`), then `check_writable_fields` (field groups), then `apply_company_scope`, then `validate_write_values`; the operation's record rule is finally compiled into the `WHERE` of the executed `INSERT … WHERE` / `UPDATE … WHERE` / `DELETE … WHERE`, so the row is touched only if the rule admits it.

The superuser (`Ctx::sudo()`) bypasses ACLs, record rules, and company scope; it remains subject only to data consistency (constraints, type validation).

### ACL: model + group grant

An `Acl` grants a **group** the four permissions on a **model** (`crates/meshble-core/src/security.rs`):

```rust
pub struct Acl {
    pub model: &'static str,
    pub group: &'static str,
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub delete: bool,
}
```

The check is **default-deny** with **union** semantics: access is granted if *at least one* of the user's groups grants the operation; the superuser is always allowed.

```rust
pub fn check_access(op: Operation, model: &str, ctx: &Ctx, acls: &[Acl]) -> bool {
    if ctx.su {
        return true;
    }
    acls.iter()
        .any(|a| a.model == model && ctx.is_member(a.group) && a.grants(op))
}
```

The `Operation`s are `Read`, `Write`, `Create`, `Delete`. A module declares its ACLs as a static slice and registers them; the server collects the union of all ACLs registered across the linked modules via `registered_acls()` (`crates/meshble-core/src/registry.rs`). The distinct groups referenced by ACLs and record rules can be derived with `registered_group_names()` (the source for seeding the read-only `res.groups` list).

A real example (module `account`): `account.user` manages entries (`account.move`) but does not delete them; configuration (creating accounts, maintaining journals, deleting entries) is reserved for `account.manager`:

```rust
pub static ACLS: &[Acl] = &[
    Acl { model: "account.account", group: "account.user", read: true, write: true, create: false, delete: false },
    Acl { model: "account.account", group: "account.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "account.journal", group: "account.user", read: true, write: false, create: false, delete: false },
    Acl { model: "account.journal", group: "account.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "account.move", group: "account.user", read: true, write: true, create: true, delete: false },
    Acl { model: "account.move", group: "account.manager", read: true, write: true, create: true, delete: true },
    // ...
];
meshble::register_acls!(ACLS);
```

### Record rules: per-row domain filters

A `RecordRule` restricts at the **row** level: it applies a typed `Domain` to the indicated operations, for the indicated groups (`crates/meshble-core/src/security.rs`):

```rust
pub struct RecordRule {
    pub model: &'static str,
    pub groups: &'static [&'static str],   // empty = global (applies to everyone)
    pub ops: &'static [Operation],
    pub domain: RuleDomain,
}
```

A rule's domain can be **static** or **already materialized**, distinguished by `RuleDomain`:

```rust
pub enum RuleDomain {
    Static(fn() -> Domain),   // static module rule (a thunk: Domain is not const-constructible)
    Owned(Domain),            // rule loaded from the DB at runtime, domain already materialized
}
```

The engine treats the two cases identically — only the domain's origin changes — so static rules and DB rules merge into a single list with no special cases (`RuleDomain::resolve` calls the thunk or clones the value).

Combining the rules applicable to `(op, model, ctx)` follows precise semantics (`record_rule_domain`): **global** rules (without a group) are *all* required → in **AND**; the rules of the groups the user belongs to are alternatives → in **OR**; the two blocks are then put in **AND**. The superuser is not restricted by any rule (`record_rule_domain` returns `None`).

A real example: the **freeze of posted accounting entries** (module `account`). The lines of a `posted` move are frozen — no write, create, or delete — so as to guarantee the invariant "posted ⇒ balanced". It's a **global** rule (`groups: &[]`) that traverses the `move_id.state` relation:

```rust
fn line_move_not_posted() -> Domain {
    Domain::field("move_id.state").ne("posted")
}

pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Write],  domain: RuleDomain::Static(line_move_not_posted) },
    RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Create], domain: RuleDomain::Static(line_move_not_posted) },
    RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Delete], domain: RuleDomain::Static(line_move_not_posted) },
];
meshble::register_rules!(RECORD_RULES);
```

The stock analog (module `stock`) is the **freeze of a validated transfer**: the lines of a `done` transfer are frozen (only sudo or a cancellation can touch them), via `picking_id.state`:

```rust
fn move_picking_not_done() -> Domain {
    Domain::field("picking_id.state").ne("done")
}

pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Write],  domain: RuleDomain::Static(move_picking_not_done) },
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Create], domain: RuleDomain::Static(move_picking_not_done) },
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Delete], domain: RuleDomain::Static(move_picking_not_done) },
];
meshble::register_rules!(RECORD_RULES);
```

The rule reaches `move_id.state` / `picking_id.state` through a dotted domain: this covers both the row's direct path and the nested `line_ids` path (writes on the children re-check the child's record rules).

### Multi-company scoping

A model is **company-scoped** when it declares a `Many2one` named `company_id`. The scope derives from the `Ctx`: `company_id` is the active company, `allowed_company_ids` the accessible set.

On **read**, `company_filter` produces the constraint (`crates/meshble-db/src/lib.rs`), with **default-deny on shared rows**:

```rust
fn company_filter(model: &ResolvedModel, ctx: &Ctx) -> Option<Domain> {
    if !ctx.company_scoped() {
        return None;                    // only sudo is unconstrained
    }
    // ... the model must have a Many2one company_id ...
    let shared = Domain::field("company_id").is_null();
    Some(if ctx.allowed_company_ids.is_empty() {
        shared // default-deny: no assignment → only shared rows (company NULL)
    } else {
        Domain::field("company_id").in_(ctx.allowed_company_ids.clone()).or(shared)
    })
}
```

A NULL `company_id` is a **shared** row, visible to every company. Any non-superuser caller is **always** constrained: with an accessible set they see those companies plus the shared rows; with an **empty** set they see only the shared rows (never "everything"). Only `sudo` is unrestricted.

On **write**, `apply_company_scope` is the single enforcement point (reused by parent create, nested child create, and update):

- an explicit `company_id` must be an id **within** the accessible set (you can't write a row into an unrelated company);
- an explicit **NULL** `company_id` is privileged (publishing a row as shared): a restricted caller cannot;
- on **create**, an unset `company_id` defaults to the caller's active company (or to the single accessible company); a restricted caller with no active company cannot create a scoped row.

#### How sudo bypasses the scope

The multi-company scope is governed by `Ctx::company_scoped()`, which is true for *any* non-superuser caller:

```rust
pub fn company_scoped(&self) -> bool {
    !self.su
}
```

So `company_filter` returns `None` (no read constraint) and `apply_company_scope` skips the restrictive checks **only** for an elevated `Ctx`. This is exactly what lets system effects (see below) read/write rows of any company when they run elevated.

### Field-level groups (`groups=`)

The `groups=` attribute on a field **hides the field** from users who don't belong to at least one of the indicated groups. It's an out-of-band restriction: it adds no columns to the metamodel, it's emitted by `#[field(groups = "...")]` as a `FieldGroupRegistration` (`crates/meshble-core/src/security.rs`). Read **and** write are gated by the same set, at the database boundary.

```rust
pub fn field_accessible(model: &str, field: &str, ctx: &Ctx) -> bool {
    if ctx.is_su() {
        return true;
    }
    match field_required_groups(model, field) {
        None => true,                                        // default-allow if no restriction
        Some(groups) => groups.iter().any(|g| ctx.is_member(g)),
    }
}
```

Enforcement is complete and symmetric:

- on **read**, unreadable fields are removed from the row (`strip_unreadable`); in addition, a caller cannot **order** by a field it cannot read (information leakage via ordering, `operation: "order by (restricted field)"`) nor **filter** on it — a client-supplied filter that references a restricted field (even through a relation, e.g. `partner_id.secret`) is rejected with `AccessDenied` (`filter_path_accessible`);
- on **write**, `check_writable_fields` rejects any payload field that the caller cannot write:

```rust
fn check_writable_fields(
    model: &ResolvedModel,
    ctx: &Ctx,
    payload: &Map<String, Json>,
) -> Result<(), DbError> {
    if ctx.is_su() {
        return Ok(());
    }
    for k in payload.keys() {
        if !field_accessible(model.name, k, ctx) {
            return Err(DbError::AccessDenied {
                model: model.name.to_string(),
                operation: "write (restricted field)",
            });
        }
    }
    Ok(())
}
```

The restriction is aware of `_inherits` delegation (a restriction on a delegated field lives on the parent model) and of shadows (a field that the child declares as its own column does not inherit the parent's restriction).

A real example (module `sales`): the structural "engine-LOCKED" fields are protected with `groups = "base.system"`, a group that no user holds, so only the generation engine (which runs `sudo`) can write them:

```rust
#[field(label = "Variant Extra Price", default = "0", groups = "base.system")]
price_extra: Decimal,

#[field(label = "On Hand", default = "0", groups = "base.system")]
qty_available: Decimal,

#[field(label = "Attribute Values", target = "product.template.attribute.value",
        relation = "variant_ptav_rel", column = "product_id", target_column = "ptav_id",
        groups = "base.system")]
product_template_attribute_value_ids: Many2many,
```

You can also declare it by hand with `meshble::register_field_groups!("res.users", "login", &["admin"]);`.

### sudo / elevated effects

`sudo` is an **explicit and typed** escalation, not a method that silently bypasses checks:

```rust
/// Returns an elevated copy that bypasses access control. Explicit and greppable.
pub fn sudo(&self) -> Ctx {
    Ctx { su: true, ..self.clone() }
}
```

The operational pattern is: **a system effect authorized by a higher-level gate**. The caller must be authorized on the high-level operation; once that gate is passed, the engine's side effects run elevated, so the user doesn't also need to hold the low-level permissions.

Two real examples in `crates/meshble-db/src/lib.rs`:

- **Invoicing** (`create_sale_invoice`): generates a posted customer invoice (`account.move`) from a confirmed order. It's gated on the caller's `Write` of the order (`check_access(Operation::Write, "sale.order", ...)`), and the order is **claimed** first (the `invoice_status` flip from `to_invoice` to `invoiced` under the caller, which enforces ACL **and** record rule + the order's company, requiring exactly one row). Only afterward does the accounting posting (GL) run elevated (`ctx.sudo()`), so a salesperson doesn't also need to hold the `account` groups. The elevated effect never starts if the caller isn't truly authorized to write the order.
- **Validating a transfer** (`validate_picking`): brings a `stock.picking` from `draft` to `done` in a transaction, moving quantities between quants. It's gated on the transfer's `Write`; the quant mutations are a system effect executed within the transaction (with `FOR UPDATE` on the picking for a true compare-and-set).

Similarly, state-transition actions have their own group gate beyond `Write`: in `run_action_secured`, if the action declares groups, a non-superuser caller must belong to them (`operation: "action (group)"` → `AccessDenied`).

## The domain AST and its operators

A `Domain` is a **typed** filter AST, validated against the model and compiled into **parameterized SQL** (`crates/meshble-core/src/domain.rs`). Values are never interpolated into the SQL text — they are bound as parameters (`$1, $2, …`) — which closes off the SQL injection surface and makes malformed filters fail at validation, not in production.

```rust
pub enum Domain {
    True,
    False,
    Cond(Condition),                  // foglia: field <op> value
    And(Box<Domain>, Box<Domain>),
    Or(Box<Domain>, Box<Domain>),
    Not(Box<Domain>),
}
```

The operators (`enum Operator`) and their fluent constructors:

| Operator | Builder | SQL | Notes |
|-----------|---------|-----|------|
| `Eq` | `.eq(v)` | `=` | `eq(Null)` becomes `IS NULL` |
| `Ne` | `.ne(v)` | `<>` | `ne(Null)` becomes `IS NOT NULL` |
| `Lt` / `Le` / `Gt` / `Ge` | `.lt` / `.le` / `.gt` / `.ge` | `<` `<=` `>` `>=` | not applicable to `Bool` fields |
| `In` / `NotIn` | `.in_(vs)` / `.not_in(vs)` | `IN` / `NOT IN` | empty list → `FALSE` / `TRUE`; `NULL` in the list is rejected |
| `Like` / `ILike` | `.like(v)` / `.ilike(v)` | `LIKE` / `ILIKE` | only on `Text` fields |
| `IsNull` / `IsNotNull` | `.is_null()` / `.is_not_null()` | `IS NULL` / `IS NOT NULL` | no bound parameter |

They combine with `.and(...)`, `.or(...)`, `.not(...)`. An example: `Domain::field("state").ne("done").and(Domain::field("amount_total").lt(10000_i64))` compiles to `(state <> $1 AND amount_total < $2)` with the values bound as parameters.

Key points of compilation:

- **identifiers from the model, never from input**: the column used in SQL is the model's `field.name`, not the incoming path string;
- **dotted paths across relations**: a `Many2one` segment becomes a subquery `fk IN (SELECT id FROM target WHERE …)`, a `One2many` becomes `id IN (SELECT inverse FROM target WHERE …)`. It works uniformly in SELECT/UPDATE/DELETE, so record rules can traverse relations;
- **NULLs handled correctly**: scalar comparisons with `NULL` are normalized (`= NULL → IS NULL`, `!= NULL → IS NOT NULL`) or rejected, and subqueries are made NULL-safe so that a `Not(...)` wrapping a traversal behaves correctly;
- **validation**: an unknown field (`UnknownField`), a non-column field such as a `One2many` (`NotAColumn`), an incompatible type (`TypeMismatch`, including NaN/Infinity on `Decimal`/`Float`), an operator unsuited to the type (`BadOperatorValue`), a non-traversable path (`UnsupportedPath`), and a relation on an unregistered model (`UnknownRelation`) are all compile/load-time errors.

The domain has a portable JSON AST (`to_json` / `from_json`): the same AST the server compiles into SQL, never an evaluated string. It's used for the `?domain=<json>` escape hatch and for record rules authored as data; the result of `from_json` stays **untrusted** and must be validated/compiled against a model before use.

## Input validation at the write boundary

Every protected create/update validates the payload in `validate_write_values` (`crates/meshble-db/src/lib.rs`):

```rust
if !field.has_column() {
    return Err(DbError::BadInput(format!("field '{key}' is not a stored column")));
}
if field.is_computed() {
    return Err(DbError::BadInput(format!("field '{key}' is computed and not writable")));
}
if jv.is_null() && field.required {
    return Err(DbError::BadInput(format!("field '{key}' is required and cannot be null")));
}
out.push((field.name, json_to_value(field, jv)?)); // type checking
```

The write-boundary guarantees are therefore:

- **required**: on create, all `required` fields (with a column, not computed) must be present; an explicit `null` value on a required field is rejected;
- **type checking**: every value is converted according to the field's type (`json_to_value`); an incompatible type is `BadInput`;
- **computed fields are not writable**: a computed field is recomputed by the engine, and attempting to write it is rejected explicitly (`'<field>' is computed and not writable`);
- **stored columns only**: writing a field that is not a stored column (relations handled separately, related) is rejected (`'<field>' is not a stored column`).

These errors are mapped to HTTP `400 Bad Request`; an `AccessDenied` becomes `403`, a conflict `409`, and internal errors an opaque `500` that exposes neither schema nor SQL (`write_error` / `internal_error` in `crates/meshble-server/src/lib.rs`).

## Guidelines for anyone writing a module

Declaring a module's security is static data, collected at compile time via the compile-time registry.

1. **ACLs** — declare a `&'static [Acl]` slice and register it. Grant the minimum per group; remember that the ACL is additive (union): adding an ACL can only **widen** access, never revoke an existing grant. Default-deny: what is not granted is denied.

   ```rust
   pub static ACLS: &[Acl] = &[
       Acl { model: "sale.order", group: "sales.user", read: true, write: true, create: true, delete: false },
       Acl { model: "sale.order.line", group: "sales.user", read: true, write: true, create: true, delete: true },
   ];
   meshble::register_acls!(ACLS);
   ```

2. **Record rules** — to restrict at the row level, declare a `&'static [RecordRule]` with `RuleDomain::Static(thunk)` and register it. Use `groups: &[]` for a **global** rule (applies to everyone, in AND), or list the groups for an alternative rule (in OR). Indicate with `ops` the operations it applies to. Leverage dotted paths (`move_id.state`) to cover both the direct path and the nested one.

   ```rust
   fn line_move_not_posted() -> Domain { Domain::field("move_id.state").ne("posted") }
   pub static RECORD_RULES: &[RecordRule] = &[
       RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Write], domain: RuleDomain::Static(line_move_not_posted) },
   ];
   meshble::register_rules!(RECORD_RULES);
   ```

3. **Field-level groups** — to hide a field from those not in the group, add `groups = "…"` to the `#[field(...)]` attribute. To lock a field down from every user (writable only by the engine via `sudo`), use a group that no one holds, such as `base.system`.

4. **Multi-company** — to make a model company-scoped, declare a `Many2one company_id`. The scoping (read, write, default on create, default-deny on shared rows) applies automatically, with no extra code.

5. **Elevated effects** — when an operation must produce a system effect (post to accounting, validate a transfer), gate it first on the caller's high-level permission and then run the effect on a `ctx.sudo()`. Keep the gate **before** the effect, so the escalation never starts without authorization.

The groups referenced by the registered ACLs and record rules are collected by `registered_group_names()` and seeded into the read-only `res.groups` list for the interface's pickers.

## Uncertainties and notes

- **JWT secret rotation (verify with the old secret)**: `MESHBLE_JWT_SECRET_OLD` is read by `Secrets::from_env` and propagated into `Secrets.jwt_secret_old` (and shown masked in the configuration summary), but `Authenticator::new` accepts a single secret and the `meshble serve` command wires only `s.secrets.jwt_secret`. In the current code, verification with the previous secret is not yet active in the runtime path; the code comment and the example files describe the intended behavior ("still accepted on verify during a rotation window"). Verify the runtime version before relying on rotation without invalidating in-flight tokens.
- **Token TTLs vs `meshble.toml`**: the effective values are the `ACCESS_TTL` / `REFRESH_TTL` constants of `meshble-server`; the `[auth]` section of `meshble.toml` (`access_ttl` / `refresh_ttl`) carries the same defaults but in v1 is not wired into token issuance. Verify the version before relying on the configuration file to change the TTLs.
- **`jti` on access tokens**: revocation by `jti` concerns only refresh tokens (`meshble_refresh` table); access tokens are stateless and not revocable before expiration (15 min). The `jti` claim is populated only on refresh tokens.
