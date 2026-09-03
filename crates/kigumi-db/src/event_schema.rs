//! The integration outbox: domain events captured at the CRUD/service seams (event_outbox), webhook
//! subscriptions (webhook_subscription), and per-subscription deliveries (webhook_delivery). This crate
//! stays HTTP-free — it only owns the QUEUE; the actual signed HTTP delivery lives at the app level
//! (the CLI), exactly like the `mail.mail` outbox. A transactional-outbox: events written on the SAME
//! transaction as the record change at the true in-tx seams are atomic with it (no event survives a
//! rolled-back mutation, none is lost on a committed one).

use crate::{is_undefined_table, Db, DbError};
use sqlx::{Postgres, Row, Transaction};

/// A domain event to enqueue. `change_summary` is small JSON metadata (changed fields, a deleted row's
/// last snapshot, a state transition) — never the full record, which the dispatcher reads fresh at
/// delivery so a webhook always reflects committed truth.
#[derive(Clone, Debug)]
pub struct OutboxEvent {
    pub event_type: String,
    pub model: String,
    pub record_id: i64,
    pub author_uid: Option<i64>,
    pub company_id: Option<i64>,
    pub change_summary: serde_json::Value,
}

/// A claimed delivery handed to the host's HTTP transport. `payload["id"]` (evt_<n>) is the stable
/// idempotency key across retries; `attempts` is the count BEFORE this attempt.
#[derive(Clone, Debug)]
pub struct WebhookDelivery {
    pub id: i64,
    pub url: String,
    pub secret: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
}

/// Retry policy: give up after this many failed attempts (dead-letter).
const WEBHOOK_MAX_ATTEMPTS: i32 = 8;
/// Backoff = base * 2^attempts, capped — 30s, 60s, 120s … up to 6h.
const WEBHOOK_BACKOFF_BASE_SECS: i64 = 30;
const WEBHOOK_BACKOFF_CAP_SECS: i64 = 21_600;

impl Db {
    /// Creates the integration tables if absent (idempotent; run at migrate + serve). Additive — never
    /// touches existing data.
    pub async fn ensure_event_schema(&self) -> Result<(), DbError> {
        for ddl in [
            "CREATE TABLE IF NOT EXISTS event_outbox (\
                id BIGSERIAL PRIMARY KEY, \
                event_type TEXT NOT NULL, \
                model TEXT NOT NULL, \
                record_id BIGINT NOT NULL, \
                author_uid BIGINT, \
                company_id BIGINT, \
                change_summary JSONB NOT NULL DEFAULT '{}'::jsonb, \
                occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                dispatched BOOLEAN NOT NULL DEFAULT false, \
                tx_id xid8 DEFAULT pg_current_xact_id())",
            "CREATE INDEX IF NOT EXISTS event_outbox_undispatched ON event_outbox (id) WHERE NOT dispatched",
            // The writer's transaction id, for GAP-SAFE cursor reads (SSE): BIGSERIAL ids are
            // assigned at INSERT time, not commit time, so id order != commit-visibility order — a
            // reader that saw id=6 must not permanently skip id=5 committed later. Readers guard
            // with tx_id < pg_snapshot_xmin(pg_current_snapshot()). NULL on pre-migration rows.
            "ALTER TABLE event_outbox ADD COLUMN IF NOT EXISTS tx_id xid8 DEFAULT pg_current_xact_id()",
            "CREATE INDEX IF NOT EXISTS event_outbox_stream ON event_outbox ((COALESCE(tx_id::text::bigint, 0)), id)",
            "CREATE TABLE IF NOT EXISTS webhook_subscription (\
                id BIGSERIAL PRIMARY KEY, \
                name TEXT NOT NULL, \
                url TEXT NOT NULL, \
                secret TEXT NOT NULL, \
                event_filter TEXT[] NOT NULL DEFAULT ARRAY['*'], \
                company_id BIGINT, \
                active BOOLEAN NOT NULL DEFAULT true, \
                created_at TIMESTAMPTZ NOT NULL DEFAULT now())",
            "CREATE TABLE IF NOT EXISTS webhook_delivery (\
                id BIGSERIAL PRIMARY KEY, \
                subscription_id BIGINT NOT NULL REFERENCES webhook_subscription(id) ON DELETE CASCADE, \
                outbox_id BIGINT NOT NULL REFERENCES event_outbox(id) ON DELETE CASCADE, \
                url TEXT NOT NULL, \
                secret TEXT NOT NULL, \
                payload JSONB NOT NULL, \
                state TEXT NOT NULL DEFAULT 'pending', \
                attempts INTEGER NOT NULL DEFAULT 0, \
                next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                lease_until TIMESTAMPTZ, \
                last_status INTEGER, \
                last_error TEXT, \
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                UNIQUE (subscription_id, outbox_id))",
            "CREATE INDEX IF NOT EXISTS webhook_delivery_due ON webhook_delivery (next_attempt_at) WHERE state = 'pending'",
        ] {
            sqlx::query(ddl).execute(&self.pool).await?;
        }
        Ok(())
    }

    /// Enqueues an event on the CALLER's transaction — atomic with the record change at the in-tx seams
    /// (a rolled-back mutation enqueues nothing). Postgres IS the queue; no trait object, no HTTP.
    /// Tolerates an unmigrated `event_outbox` (the integration schema not yet created).
    pub async fn enqueue_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        ev: &OutboxEvent,
    ) -> Result<(), DbError> {
        sqlx::query("SAVEPOINT kigumi_enqueue").execute(&mut **tx).await?;
        let r = sqlx::query(
            "INSERT INTO event_outbox (event_type, model, record_id, author_uid, company_id, change_summary) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&ev.event_type)
        .bind(&ev.model)
        .bind(ev.record_id)
        .bind(ev.author_uid)
        .bind(ev.company_id)
        .bind(&ev.change_summary)
        .execute(&mut **tx)
        .await;
        match r {
            Ok(_) => {
                sqlx::query("RELEASE SAVEPOINT kigumi_enqueue").execute(&mut **tx).await?;
                Ok(())
            }
            // Same reasoning as exec_tolerant_in_tx: swallowing the error without rolling back to a
            // savepoint would return Ok on a transaction Postgres has already aborted, and the
            // caller's next statement would fail with something unrelated.
            Err(e) if is_undefined_table(&e) => {
                sqlx::query("ROLLBACK TO SAVEPOINT kigumi_enqueue").execute(&mut **tx).await?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Best-effort post-commit enqueue (its own connection) for the seams that are NOT a single
    /// transaction — at-least-once but loseable in the commit→INSERT crash window. `delete_secured`
    /// has since moved to the in-tx path; the remaining users are the module-owned `post_move` and
    /// `register_payment` service seams, whose atomicity is F3 work on the module side. Documented per
    /// seam; never silently mixed with the in-tx path.
    pub async fn enqueue_event(&self, ev: &OutboxEvent) -> Result<(), DbError> {
        let r = sqlx::query(
            "INSERT INTO event_outbox (event_type, model, record_id, author_uid, company_id, change_summary) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&ev.event_type)
        .bind(&ev.model)
        .bind(ev.record_id)
        .bind(ev.author_uid)
        .bind(ev.company_id)
        .bind(&ev.change_summary)
        .execute(&self.pool)
        .await;
        match r {
            Ok(_) => Ok(()),
            Err(e) if is_undefined_table(&e) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Fans undispatched events out to matching active subscriptions: one `webhook_delivery` per
    /// (subscription, event), with a FROZEN thin envelope payload (id/type/model/record_id/changes — the
    /// consumer GETs the full record if it wants details). Crash-safe exactly-once via UNIQUE
    /// (subscription_id, outbox_id) + ON CONFLICT DO NOTHING, then the events are marked dispatched. A
    /// subscription matches by event_filter ('*' or the exact type) and company (NULL subscription = all
    /// companies; a company-less event reaches every subscription). Returns deliveries created.
    pub async fn fan_out_events(&self) -> Result<u64, DbError> {
        let mut tx = self.pool.begin().await?;
        let inserted = match sqlx::query(
            "INSERT INTO webhook_delivery (subscription_id, outbox_id, url, secret, payload) \
             SELECT s.id, e.id, s.url, s.secret, jsonb_build_object( \
                 'id', 'evt_' || e.id, 'type', e.event_type, 'api_version', 'v1', \
                 'occurred_at', e.occurred_at, 'actor', jsonb_build_object('uid', e.author_uid), \
                 'company_id', e.company_id, 'model', e.model, 'record_id', e.record_id, \
                 'changes', e.change_summary) \
             FROM event_outbox e \
             JOIN webhook_subscription s ON s.active \
               AND (s.event_filter @> ARRAY['*'] OR s.event_filter @> ARRAY[e.event_type]) \
               AND (s.company_id IS NULL OR e.company_id IS NULL OR s.company_id = e.company_id) \
             WHERE NOT e.dispatched \
             ON CONFLICT (subscription_id, outbox_id) DO NOTHING",
        )
        .execute(&mut *tx)
        .await
        {
            Ok(r) => r.rows_affected(),
            Err(e) if is_undefined_table(&e) => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        sqlx::query("UPDATE event_outbox SET dispatched = true WHERE NOT dispatched")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(inserted)
    }

    /// Registers a webhook subscription (admin). The secret is supplied by the caller (generated
    /// server-side, shown once). Returns the new id.
    pub async fn create_webhook_subscription(
        &self,
        name: &str,
        url: &str,
        secret: &str,
        event_filter: &[String],
        company_id: Option<i64>,
    ) -> Result<i64, DbError> {
        let filter: Vec<String> = if event_filter.is_empty() {
            vec!["*".to_string()]
        } else {
            event_filter.to_vec()
        };
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO webhook_subscription (name, url, secret, event_filter, company_id) \
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(name)
        .bind(url)
        .bind(secret)
        .bind(&filter)
        .bind(company_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// Lists subscriptions WITHOUT the secret (never returned after creation).
    pub async fn list_webhook_subscriptions(&self) -> Result<Vec<serde_json::Value>, DbError> {
        let rows = sqlx::query(
            "SELECT id, name, url, event_filter, company_id, active, created_at::text FROM webhook_subscription ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<i64, _>("id").unwrap_or_default(),
                    "name": r.try_get::<String, _>("name").unwrap_or_default(),
                    "url": r.try_get::<String, _>("url").unwrap_or_default(),
                    "event_filter": r.try_get::<Vec<String>, _>("event_filter").unwrap_or_default(),
                    "company_id": r.try_get::<Option<i64>, _>("company_id").ok().flatten(),
                    "active": r.try_get::<bool, _>("active").unwrap_or(false),
                    "created_at": r.try_get::<Option<String>, _>("created_at").ok().flatten(),
                })
            })
            .collect())
    }

    /// Deactivates a subscription (no more fan-out to it). Returns true if a row was updated.
    pub async fn deactivate_webhook_subscription(&self, id: i64) -> Result<bool, DbError> {
        let n = sqlx::query("UPDATE webhook_subscription SET active = false WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Delivers pending+due webhook deliveries via `send` (the host's signed HTTP transport), with an
    /// atomic claim-and-advance (pending -> delivering with a lease, the run_due_crons SKIP LOCKED shape:
    /// the lock is NOT held across the POST). On success: sent. On failure: re-queued with exponential
    /// backoff (attempts++), or dead-lettered after WEBHOOK_MAX_ATTEMPTS. The transport stays at the app
    /// level (reqwest+HMAC in the CLI); this crate has no HTTP dep. Returns the number delivered.
    pub async fn flush_webhooks(
        &self,
        send: &(dyn Fn(&WebhookDelivery) -> Result<(), String> + Send + Sync),
    ) -> Result<usize, DbError> {
        let claimed = match sqlx::query(
            "UPDATE webhook_delivery SET state = 'delivering', lease_until = now() + interval '5 minutes' \
             WHERE id IN ( \
                 SELECT id FROM webhook_delivery WHERE state = 'pending' AND next_attempt_at <= now() \
                 ORDER BY next_attempt_at LIMIT 100 FOR UPDATE SKIP LOCKED) \
             RETURNING id, url, secret, payload, attempts",
        )
        .fetch_all(&self.pool)
        .await
        {
            Ok(r) => r,
            Err(e) if is_undefined_table(&e) => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let mut delivered = 0usize;
        for r in &claimed {
            let d = WebhookDelivery {
                id: r.try_get("id")?,
                url: r.try_get::<Option<String>, _>("url").ok().flatten().unwrap_or_default(),
                secret: r.try_get::<Option<String>, _>("secret").ok().flatten().unwrap_or_default(),
                payload: r.try_get::<serde_json::Value, _>("payload").unwrap_or(serde_json::Value::Null),
                attempts: r.try_get::<i32, _>("attempts").unwrap_or(0),
            };
            match send(&d) {
                Ok(()) => {
                    sqlx::query("UPDATE webhook_delivery SET state = 'sent', lease_until = NULL, last_status = 200, last_error = NULL WHERE id = $1")
                        .bind(d.id)
                        .execute(&self.pool)
                        .await?;
                    delivered += 1;
                }
                Err(err) => {
                    let attempts = d.attempts + 1;
                    let err = err.chars().take(500).collect::<String>(); // never log a secret/large body
                    if attempts >= WEBHOOK_MAX_ATTEMPTS {
                        sqlx::query("UPDATE webhook_delivery SET state = 'dead', attempts = $2, lease_until = NULL, last_error = $3 WHERE id = $1")
                            .bind(d.id)
                            .bind(attempts)
                            .bind(&err)
                            .execute(&self.pool)
                            .await?;
                    } else {
                        let backoff = (WEBHOOK_BACKOFF_BASE_SECS.saturating_mul(1i64 << attempts.min(20)))
                            .min(WEBHOOK_BACKOFF_CAP_SECS);
                        sqlx::query("UPDATE webhook_delivery SET state = 'pending', attempts = $2, lease_until = NULL, last_error = $3, next_attempt_at = now() + make_interval(secs => $4) WHERE id = $1")
                            .bind(d.id)
                            .bind(attempts)
                            .bind(&err)
                            .bind(backoff as f64)
                            .execute(&self.pool)
                            .await?;
                    }
                }
            }
        }
        Ok(delivered)
    }

    /// Re-queues deliveries stuck in 'delivering' past their lease (a flusher crashed mid-send) so they
    /// are retried. Run on a cron.
    pub async fn reap_stuck_deliveries(&self) -> Result<u64, DbError> {
        let n = match sqlx::query("UPDATE webhook_delivery SET state = 'pending', lease_until = NULL WHERE state = 'delivering' AND lease_until < now()")
            .execute(&self.pool)
            .await
        {
            Ok(r) => r.rows_affected(),
            Err(e) if is_undefined_table(&e) => 0,
            Err(e) => return Err(e.into()),
        };
        Ok(n)
    }

    /// Test helper: clears the backoff so pending deliveries are immediately due (lets a test drive the
    /// retry loop without sleeping through the exponential backoff).
    pub async fn force_deliveries_due(&self) -> Result<u64, DbError> {
        Ok(sqlx::query("UPDATE webhook_delivery SET next_attempt_at = now() WHERE state = 'pending'")
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    /// Count of deliveries in a given state (test helper).
    pub async fn deliveries_in_state(&self, state: &str) -> Result<i64, DbError> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM webhook_delivery WHERE state = $1")
            .bind(state)
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0))
    }

    /// Empties the integration outbox + deliveries (test/admin helper).
    pub async fn clear_event_outbox(&self) -> Result<(), DbError> {
        let _ = sqlx::query("TRUNCATE event_outbox RESTART IDENTITY CASCADE").execute(&self.pool).await;
        Ok(())
    }

    /// Empties webhook subscriptions + their deliveries (test helper — subscriptions are not migration
    /// models, so a test's table-drop does not reset them).
    pub async fn clear_webhook_subscriptions(&self) -> Result<(), DbError> {
        let _ = sqlx::query("TRUNCATE webhook_subscription RESTART IDENTITY CASCADE").execute(&self.pool).await;
        Ok(())
    }

    /// The number of undispatched events (test/observability helper).
    pub async fn outbox_pending_count(&self) -> Result<i64, DbError> {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE NOT dispatched")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);
        Ok(n)
    }

    /// Reads recent events for a model+record (test/audit helper), newest first.
    pub async fn events_for(&self, model: &str, record_id: i64) -> Result<Vec<serde_json::Value>, DbError> {
        let rows = sqlx::query(
            "SELECT id, event_type, model, record_id, change_summary FROM event_outbox \
             WHERE model = $1 AND record_id = $2 ORDER BY id DESC",
        )
        .bind(model)
        .bind(record_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<i64, _>("id").unwrap_or_default(),
                    "event_type": r.try_get::<String, _>("event_type").unwrap_or_default(),
                    "model": r.try_get::<String, _>("model").unwrap_or_default(),
                    "record_id": r.try_get::<i64, _>("record_id").unwrap_or_default(),
                    "change_summary": r.try_get::<serde_json::Value, _>("change_summary").unwrap_or(serde_json::Value::Null),
                })
            })
            .collect())
    }
}

/// One outbox row as the live event stream reads it — the SSE counterpart of the webhook envelope.
#[derive(Clone, Debug)]
pub struct StoredEvent {
    pub id: i64,
    /// The writer transaction's xid8 as a number (0 for pre-migration rows) — the FIRST component
    /// of the stream cursor: rows below the running-xid horizon form a set that only grows in
    /// (txn, id) order, so a (txn, id) cursor can never skip (an id-only cursor CAN: a long tx has
    /// a low xid but late, high ids).
    pub txn: i64,
    pub event_type: String,
    pub model: String,
    pub record_id: i64,
    pub author_uid: Option<i64>,
    pub company_id: Option<i64>,
    pub change_summary: serde_json::Value,
    pub occurred_at: String,
}

impl Db {
    /// The highest DELIVERABLE (txn, id) cursor — the "from now on" point for a new stream. Applies
    /// the SAME running-xid guard as [`Db::events_after`]: a row whose writer is still uncommitted
    /// must stay ABOVE the cursor, or it would be skipped forever once that writer commits.
    pub async fn latest_event_cursor(&self) -> Result<(i64, i64), DbError> {
        let row = sqlx::query(
            "SELECT COALESCE(tx_id::text::bigint, 0) AS txn, id FROM event_outbox \
             WHERE tx_id IS NULL OR tx_id < pg_snapshot_xmin(pg_current_snapshot()) \
             ORDER BY COALESCE(tx_id::text::bigint, 0) DESC, id DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get::<i64, _>("txn"), r.get::<i64, _>("id"))).unwrap_or((0, 0)))
    }

    /// Events after the `(txn, id)` cursor, GAP-SAFE. Two properties compose: (1) only rows whose
    /// writer transaction is below every RUNNING transaction are eligible (`tx_id <
    /// pg_snapshot_xmin(pg_current_snapshot())`) — the eligible set is stable and only ever grows;
    /// (2) it grows in (txn, id) order, so a (txn, id) cursor can never skip. An id-only cursor is
    /// NOT safe: a long transaction carries the xid of its FIRST write but inserts events late with
    /// HIGH ids — an overlapping younger writer's lower id would fall behind the cursor forever.
    /// The trade-off is delivery lag bounded by the longest concurrent write transaction.
    /// Pre-migration rows (tx_id NULL) read as txn 0 — a legacy prefix.
    pub async fn events_after(&self, cursor: (i64, i64), limit: i64) -> Result<Vec<StoredEvent>, DbError> {
        let rows = sqlx::query(
            "SELECT id, COALESCE(tx_id::text::bigint, 0) AS txn, event_type, model, record_id, author_uid, company_id, change_summary, occurred_at::text AS occurred_at \
             FROM event_outbox \
             WHERE (COALESCE(tx_id::text::bigint, 0), id) > ($1, $2) \
               AND (tx_id IS NULL OR tx_id < pg_snapshot_xmin(pg_current_snapshot())) \
             ORDER BY COALESCE(tx_id::text::bigint, 0), id LIMIT $3",
        )
        .bind(cursor.0)
        .bind(cursor.1)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| StoredEvent {
                id: r.get("id"),
                txn: r.get("txn"),
                event_type: r.get("event_type"),
                model: r.get("model"),
                record_id: r.get("record_id"),
                author_uid: r.try_get("author_uid").ok(),
                company_id: r.try_get("company_id").ok(),
                change_summary: r.try_get("change_summary").unwrap_or(serde_json::Value::Null),
                occurred_at: r.try_get("occurred_at").unwrap_or_default(),
            })
            .collect())
    }

    /// Filters a batch of events down to what `ctx` may SEE, and shapes each into the stream JSON.
    /// Read ACL first (in memory); then each live-record event re-reads its row through
    /// `find_one_secured` — THE visibility path every read uses (record rules + company scope) —
    /// memoized per (model, record) within the batch. (`id` is not domain-addressable today, so a
    /// batched id-IN secured query is not expressible; batches are small — one poll tick.)
    /// `model.deleted` events cannot re-read the row, so they gate on the Read ACL, the event's
    /// company against the caller's scope (NULL = shared = visible; default-deny otherwise, like
    /// `company_filter`) — AND are suppressed entirely when a Read record rule applies to this
    /// caller on the model (the rule cannot be evaluated against a gone row; default-deny, matching
    /// the posture everywhere else). `changed_fields` names are filtered per field ACL (D6), and a
    /// `model.state_changed` whose `state` field is D6-restricted for this caller has its
    /// field/from/to values blanked — record visibility must not leak restricted VALUES. Custom
    /// service events (`ServiceCtx::emit_event`) pass their summary through: it is visible to every
    /// caller who can see the record (documented on emit_event).
    pub async fn visible_events(
        &self,
        ctx: &kigumi_core::Ctx,
        acls: &[kigumi_core::Acl],
        rules: &[kigumi_core::RecordRule],
        events: &[StoredEvent],
    ) -> Result<Vec<serde_json::Value>, DbError> {
        use kigumi_core::{check_access, field_accessible, resolve_registered, Operation};
        use std::collections::BTreeMap;

        let mut memo: BTreeMap<(String, i64), bool> = BTreeMap::new();
        let mut out = Vec::new();
        for ev in events {
            let seen = if ev.event_type == "model.deleted" {
                check_access(Operation::Read, &ev.model, ctx, acls)
                    && kigumi_core::record_rule_domain(Operation::Read, &ev.model, ctx, rules).is_none()
                    && (ctx.is_su() || ev.company_id.is_none_or(|c| ctx.allowed_company_ids.contains(&c)))
            } else {
                let key = (ev.model.clone(), ev.record_id);
                match memo.get(&key) {
                    Some(v) => *v,
                    None => {
                        let v = if check_access(Operation::Read, &ev.model, ctx, acls) {
                            match resolve_registered(&ev.model) {
                                Ok(model) => {
                                    self.find_one_secured(&model, ctx, acls, rules, ev.record_id).await?.is_some()
                                }
                                Err(_) => false,
                            }
                        } else {
                            false
                        };
                        memo.insert(key, v);
                        v
                    }
                }
            };
            if !seen {
                continue;
            }
            // D6: drop restricted field NAMES from the change summary for this caller — and for a
            // state transition on a D6-restricted `state`, blank the VALUES too (record visibility
            // must not leak from/to of a field the caller cannot read).
            let mut changes = ev.change_summary.clone();
            if let Some(fields) = changes.get_mut("changed_fields").and_then(|v| v.as_array_mut()) {
                fields.retain(|f| f.as_str().is_some_and(|name| field_accessible(&ev.model, name, ctx)));
            }
            if ev.event_type == "model.state_changed" && !field_accessible(&ev.model, "state", ctx) {
                changes = serde_json::json!({});
            }
            out.push(serde_json::json!({
                "id": ev.id,
                "txn": ev.txn,
                "type": ev.event_type,
                "model": ev.model,
                "record_id": ev.record_id,
                "actor": { "uid": ev.author_uid },
                "company_id": ev.company_id,
                "occurred_at": ev.occurred_at,
                "changes": changes,
            }));
        }
        Ok(out)
    }
}
