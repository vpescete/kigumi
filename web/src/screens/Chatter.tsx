import { useCallback, useEffect, useState } from 'react'
import { Bell, BellOff, Check, MessageSquare, Send } from 'lucide-react'
import * as api from '../api'
import type { Activity, Follower, Message } from '../api'
import { useAuth } from '../auth'
import { Badge, Button, Card, Loading } from '../ui'

const STATE_TONE: Record<api.ActivityState, 'danger' | 'warning' | 'neutral'> = {
  overdue: 'danger',
  today: 'warning',
  planned: 'neutral',
}

// The chatter for one record: its message thread (comments + system audit), open activities, and the
// caller's follow toggle. Rendered by ModelForm for any `mailed` model. Every call is gated server-side
// on read access to the host record, so this widget needs no extra permission logic.
export function Chatter({ model, id }: { model: string; id: number }) {
  const { identity } = useAuth()
  const [messages, setMessages] = useState<Message[]>([])
  const [acts, setActs] = useState<Activity[]>([])
  const [followers, setFollowers] = useState<Follower[]>([])
  const [body, setBody] = useState('')
  const [summary, setSummary] = useState('')
  const [deadline, setDeadline] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [loading, setLoading] = useState(true)

  const load = useCallback(async (): Promise<void> => {
    setError(null)
    try {
      const [m, a, f] = await Promise.all([
        api.messages(model, id),
        api.activities(model, id),
        api.followers(model, id),
      ])
      setMessages(m)
      setActs(a)
      setFollowers(f)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load chatter')
    } finally {
      setLoading(false)
    }
  }, [model, id])

  useEffect(() => {
    void load()
  }, [load])

  const isFollowing = identity != null && followers.some((f) => f.user_id === identity.uid)

  async function run(fn: () => Promise<void>): Promise<void> {
    setBusy(true)
    setError(null)
    try {
      await fn()
      await load()
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Action failed')
    } finally {
      setBusy(false)
    }
  }

  const post = (type: 'comment' | 'note') =>
    run(async () => {
      if (!body.trim()) return
      await api.postMessage(model, id, body, type)
      setBody('')
    })

  const schedule = () =>
    run(async () => {
      if (!summary.trim()) return
      await api.scheduleActivity(model, id, summary, deadline || undefined)
      setSummary('')
      setDeadline('')
    })

  if (loading) {
    return (
      <Card className="p-5">
        <Loading label="Loading chatter…" />
      </Card>
    )
  }

  return (
    <Card className="p-5">
      <div className="flex items-center justify-between mb-4">
        <h2 className="t-h2 text-text flex items-center gap-2">
          <MessageSquare size={16} /> Chatter
        </h2>
        <div className="flex items-center gap-3">
          <span className="t-label text-muted">
            {followers.length} {followers.length === 1 ? 'follower' : 'followers'}
          </span>
          <Button
            variant="secondary"
            icon={isFollowing ? <BellOff size={15} /> : <Bell size={15} />}
            onClick={() => run(() => api.setFollow(model, id, !isFollowing))}
            disabled={busy}
          >
            {isFollowing ? 'Unfollow' : 'Follow'}
          </Button>
        </div>
      </div>

      {error && <div className="t-body text-danger bg-danger-bg rounded-md px-3 py-2 mb-4">{error}</div>}

      <ActivitySection
        acts={acts}
        summary={summary}
        deadline={deadline}
        busy={busy}
        onSummary={setSummary}
        onDeadline={setDeadline}
        onSchedule={schedule}
        onDone={(aid) => run(() => api.activityDone(model, id, aid))}
      />

      <Composer body={body} busy={busy} onBody={setBody} onComment={() => post('comment')} onNote={() => post('note')} />

      <div className="mt-5 flex flex-col gap-4">
        {messages.length === 0 ? (
          <div className="t-body text-muted">No messages yet.</div>
        ) : (
          messages.map((m) => <MessageItem key={m.id} m={m} />)
        )}
      </div>
    </Card>
  )
}

function ActivitySection({
  acts,
  summary,
  deadline,
  busy,
  onSummary,
  onDeadline,
  onSchedule,
  onDone,
}: {
  acts: Activity[]
  summary: string
  deadline: string
  busy: boolean
  onSummary: (v: string) => void
  onDeadline: (v: string) => void
  onSchedule: () => void
  onDone: (aid: number) => void
}) {
  const input = 'px-3 rounded-md bg-surface2 border border-border text-text focus:outline-none focus:ring-2 focus:ring-[var(--color-ring)]'
  return (
    <div className="mb-4 rounded-md border border-border p-3">
      <div className="t-label text-muted mb-2">Activities</div>
      {acts.length > 0 && (
        <div className="flex flex-col gap-1.5 mb-3">
          {acts.map((a) => (
            <div key={a.id} className="flex items-center gap-2 t-body">
              <Badge tone={STATE_TONE[a.state]}>{a.state}</Badge>
              <span className="text-text">{a.summary}</span>
              {a.date_deadline && <span className="text-muted">· {a.date_deadline}</span>}
              <button
                onClick={() => onDone(a.id)}
                disabled={busy}
                className="ml-auto inline-flex items-center gap-1 text-muted hover:text-success disabled:opacity-50"
                title="Mark done"
              >
                <Check size={15} />
              </button>
            </div>
          ))}
        </div>
      )}
      <div className="flex gap-2">
        <input
          value={summary}
          onChange={(e) => onSummary(e.target.value)}
          placeholder="Schedule an activity…"
          className={`${input} flex-1`}
          style={{ height: 'var(--control-h)' }}
        />
        <input
          type="date"
          value={deadline}
          onChange={(e) => onDeadline(e.target.value)}
          className={input}
          style={{ height: 'var(--control-h)' }}
        />
        <Button variant="secondary" onClick={onSchedule} disabled={busy || !summary.trim()}>
          Add
        </Button>
      </div>
    </div>
  )
}

function Composer({
  body,
  busy,
  onBody,
  onComment,
  onNote,
}: {
  body: string
  busy: boolean
  onBody: (v: string) => void
  onComment: () => void
  onNote: () => void
}) {
  return (
    <div>
      <textarea
        value={body}
        onChange={(e) => onBody(e.target.value)}
        placeholder="Write a message…"
        rows={2}
        className="w-full px-3 py-2 rounded-md bg-surface2 border border-border text-text focus:outline-none focus:ring-2 focus:ring-[var(--color-ring)]"
      />
      <div className="flex gap-2 mt-2">
        <Button variant="primary" icon={<Send size={15} />} onClick={onComment} disabled={busy || !body.trim()}>
          Send
        </Button>
        <Button variant="secondary" onClick={onNote} disabled={busy || !body.trim()}>
          Log note
        </Button>
      </div>
    </div>
  )
}

function MessageItem({ m }: { m: Message }) {
  const who = m.author_id != null ? `User ${m.author_id}` : 'System'
  const when = m.date ? m.date.slice(0, 16) : ''
  const isAudit = m.message_type === 'notification'
  return (
    <div className="flex gap-3">
      <div className="h-7 w-7 shrink-0 rounded-full bg-surface2 border border-border flex items-center justify-center t-label text-muted">
        {m.author_id ?? '·'}
      </div>
      <div className="min-w-0 flex-1">
        <div className="t-label text-muted">
          <span className="text-text">{who}</span> · {when}
          {m.message_type === 'note' && <span className="ml-1">(note)</span>}
        </div>
        {isAudit ? (
          <div className="t-body text-muted">
            {m.tracking.length === 0 ? (
              <em>updated</em>
            ) : (
              m.tracking.map((t) => (
                <div key={t.field}>
                  <span className="text-text">{t.field}</span>: {t.old_value ?? '—'} → {t.new_value ?? '—'}
                </div>
              ))
            )}
          </div>
        ) : (
          <div className="t-body text-text whitespace-pre-wrap">{m.body}</div>
        )}
      </div>
    </div>
  )
}
