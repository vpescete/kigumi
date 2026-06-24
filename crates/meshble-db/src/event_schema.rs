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
                dispatched BOOLEAN NOT NULL DEFAULT false)",
            "CREATE INDEX IF NOT EXISTS event_outbox_undispatched ON event_outbox (id) WHERE NOT dispatched",
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
            Ok(_) => Ok(()),
            Err(e) if is_undefined_table(&e) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Best-effort post-commit enqueue (its own connection) for the seams that are NOT a single
    /// transaction (delete_secured / post_move / register_payment as the code stands) — at-least-once but
    /// loseable in the commit→INSERT crash window until the Phase-4 transactional wrapping. Documented per
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

    /// Empties the integration outbox + deliveries (test/admin helper).
    pub async fn clear_event_outbox(&self) -> Result<(), DbError> {
        let _ = sqlx::query("TRUNCATE event_outbox RESTART IDENTITY CASCADE").execute(&self.pool).await;
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
