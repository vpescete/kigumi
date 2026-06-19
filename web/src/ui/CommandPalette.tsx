// ⌘K command palette — a controlled overlay that renders sections of commands, fuzzy-filters them by a
// query, and runs the selected one. Pure UI + keyboard: the host (App) supplies the sections (navigate
// to a model, quick actions) and may add async results via onQuery (e.g. matching records).

import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Search } from 'lucide-react'
import { Portal, useDismiss, useFocusTrap } from './overlay'
import { cx } from './cx'

export interface CommandItem {
  id: string
  label: string
  hint?: string
  icon?: ReactNode
  run: () => void
}
export interface CommandSection {
  title: string
  items: CommandItem[]
}

/** Subsequence fuzzy score (null = no match); rewards contiguous runs and word-boundary starts. */
function fuzzyScore(q: string, text: string): number | null {
  if (!q) return 0
  const ql = q.toLowerCase()
  const tl = text.toLowerCase()
  let qi = 0
  let score = 0
  let run = 0
  let prev = -2
  for (let ti = 0; ti < tl.length && qi < ql.length; ti++) {
    if (tl[ti] === ql[qi]) {
      run = ti === prev + 1 ? run + 1 : 1
      const boundary = ti === 0 || /[\W_]/.test(tl[ti - 1])
      score += run + (boundary ? 3 : 0)
      prev = ti
      qi++
    }
  }
  return qi === ql.length ? score : null
}

export function CommandPalette({
  open,
  onClose,
  sections,
  onQuery,
  placeholder = 'Search models, records, actions…',
}: {
  open: boolean
  onClose: () => void
  sections: CommandSection[]
  onQuery?: (query: string) => void
  placeholder?: string
}) {
  const [query, setQuery] = useState('')
  const [active, setActive] = useState(0)
  const ref = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  useFocusTrap(ref, open)
  useDismiss(ref, open, onClose)

  useEffect(() => {
    if (open) {
      setQuery('')
      setActive(0)
      setTimeout(() => inputRef.current?.focus(), 0)
    }
  }, [open])

  // Filter + rank each section; flatten the visible items for keyboard navigation.
  const visible = useMemo(() => {
    return sections
      .map((s) => ({
        title: s.title,
        items: s.items
          .map((it) => ({ it, score: fuzzyScore(query.trim(), it.label) }))
          .filter((x): x is { it: CommandItem; score: number } => x.score !== null)
          .sort((a, b) => b.score - a.score)
          .map((x) => x.it),
      }))
      .filter((s) => s.items.length > 0)
  }, [sections, query])
  const flat = useMemo(() => visible.flatMap((s) => s.items), [visible])

  useEffect(() => setActive(0), [query])
  useEffect(() => {
    if (active >= flat.length) setActive(Math.max(0, flat.length - 1))
  }, [flat.length, active])

  if (!open) return null

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setActive((a) => Math.min(a + 1, flat.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setActive((a) => Math.max(a - 1, 0))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      const item = flat[active]
      if (item) {
        onClose()
        item.run()
      }
    }
  }

  let idx = -1
  return (
    <Portal>
      <div className="fixed inset-0 z-overlay flex items-start justify-center overflow-y-auto bg-bg/70 p-4 backdrop-blur-sm sm:pt-[12vh]">
        <div
          ref={ref}
          role="dialog"
          aria-modal="true"
          aria-label="Command palette"
          tabIndex={-1}
          className="msh-dialog-in relative z-dialog w-full max-w-xl overflow-hidden rounded-lg border border-border bg-surface shadow-overlay"
        >
          <div className="flex items-center gap-2 border-b border-border px-3.5">
            <Search size={16} className="shrink-0 text-muted" aria-hidden="true" />
            <input
              ref={inputRef}
              role="combobox"
              aria-expanded
              aria-controls="msh-cmd-list"
              aria-activedescendant={flat[active] ? `msh-cmd-${active}` : undefined}
              className="t-subtitle h-12 min-w-0 flex-1 bg-transparent text-text outline-none placeholder:text-muted"
              placeholder={placeholder}
              value={query}
              onChange={(e) => {
                setQuery(e.target.value)
                onQuery?.(e.target.value)
              }}
              onKeyDown={onKey}
            />
            <kbd className="t-mono shrink-0 rounded-sm border border-border px-1.5 py-0.5 text-[10px] text-muted">esc</kbd>
          </div>
          <ul id="msh-cmd-list" role="listbox" className="max-h-[min(60vh,420px)] overflow-auto py-1.5">
            {flat.length === 0 && <li className="t-body px-4 py-6 text-center text-muted">No matches for &ldquo;{query}&rdquo;</li>}
            {visible.map((s) => (
              <li key={s.title} role="presentation">
                <div className="t-label px-4 pb-1 pt-2 text-muted">{s.title}</div>
                <ul role="group">
                  {s.items.map((it) => {
                    idx += 1
                    const i = idx
                    return (
                      <li
                        key={it.id}
                        id={`msh-cmd-${i}`}
                        role="option"
                        aria-selected={i === active}
                        onMouseEnter={() => setActive(i)}
                        onMouseDown={(e) => {
                          e.preventDefault()
                          onClose()
                          it.run()
                        }}
                        className={cx(
                          'flex cursor-pointer items-center gap-2.5 border-l-2 px-4 py-2',
                          i === active ? 'border-accent bg-accent-soft' : 'border-transparent',
                        )}
                      >
                        {it.icon && <span className={cx('shrink-0', i === active ? 'text-accent' : 'text-muted')}>{it.icon}</span>}
                        <span className="t-body min-w-0 flex-1 truncate text-text">{it.label}</span>
                        {it.hint && <span className="t-caption shrink-0 text-muted">{it.hint}</span>}
                      </li>
                    )
                  })}
                </ul>
              </li>
            ))}
          </ul>
        </div>
      </div>
    </Portal>
  )
}
