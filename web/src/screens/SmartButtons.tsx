// The smart-button row on a record form: stat tiles that show a related count (or a record value) and
// link to the filtered list. A cleaner take on Odoo's button box — spacious tiles on the design system.
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import * as api from '../api'
import { cx, focusRing } from '../ui'
import { displayText, modelTitle } from '../format'
import { SMART_BUTTONS, type SmartButton } from '../registries/smartButtons'

type Tile = { btn: SmartButton; value: string; link?: { target: string; domain: unknown; label: string } }

export function SmartButtons({ model, recordId, record }: { model: string; recordId: number; record: api.Row | null }) {
  const nav = useNavigate()
  const [tiles, setTiles] = useState<Tile[] | null>(null)
  const buttons = SMART_BUTTONS[model] ?? []

  useEffect(() => {
    let active = true
    if (buttons.length === 0) {
      setTiles([])
      return
    }
    const recordLabel = (record?.name as string) || `${modelTitle(model)} #${recordId}`
    async function build(): Promise<void> {
      const out = await Promise.all(
        buttons.map(async (btn): Promise<Tile | null> => {
          if (btn.source.kind === 'count') {
            const { target, field } = btn.source
            const domain = { field, op: '=', value: recordId }
            try {
              const total = await api.list(target, { domain, limit: 1 }).then((p) => p.total)
              return { btn, value: String(total), link: { target, domain, label: `${btn.label} · ${recordLabel}` } }
            } catch {
              return null // target model not installed / not permitted — drop the tile
            }
          }
          // A value read off the record; link (if any) to the filtered related list.
          const raw = record?.[btn.source.name]
          const value = displayText(raw, btn.source.widget ?? 'char')
          const link = btn.link
            ? { target: btn.link.target, domain: { field: btn.link.field, op: '=', value: recordId }, label: `${btn.label} · ${recordLabel}` }
            : undefined
          return { btn, value, link }
        }),
      )
      if (active) setTiles(out.filter((t): t is Tile => t !== null))
    }
    void build()
    return () => {
      active = false
    }
  }, [model, recordId, record, buttons])

  if (!tiles || tiles.length === 0) return null

  const go = (t: Tile): void => {
    if (!t.link) return
    const params = new URLSearchParams({ domain: JSON.stringify(t.link.domain), label: t.link.label })
    nav(`/m/${t.link.target}?${params.toString()}`)
  }

  return (
    <div className="mb-5 flex flex-wrap gap-3">
      {tiles.map((t, i) => (
        <button
          key={i}
          type="button"
          disabled={!t.link}
          onClick={() => go(t)}
          className={cx(
            'flex min-w-[9rem] items-center gap-3 rounded-lg border border-border bg-surface px-4 py-2.5 text-left shadow-xs',
            t.link
              ? 'transition-[box-shadow,border-color] duration-base ease-out hover:border-input-border hover:shadow-md'
              : 'cursor-default',
            focusRing,
          )}
        >
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-md bg-surface2 text-accent">{t.btn.icon}</span>
          <span className="min-w-0">
            <span className="t-display block text-[1.4rem] leading-none tabular-nums text-text">{t.value}</span>
            <span className="t-caption mt-1 block truncate text-muted">{t.btn.label}</span>
          </span>
        </button>
      ))}
    </div>
  )
}
