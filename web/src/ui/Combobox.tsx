// A searchable single-select (for Many2one pickers) — replaces a native <select> that becomes unusable
// past a few dozen options. Client-filters by label; full keyboard support; reads field-input tokens.

import { useEffect, useId, useMemo, useRef, useState } from 'react'
import { ChevronsUpDown, X } from 'lucide-react'
import { useDismiss } from './overlay'
import { cx, focusRing } from './cx'

export interface ComboOption {
  value: number | string
  label: string
}

export function Combobox({
  value,
  onChange,
  options,
  placeholder = 'Select…',
  allowClear = true,
  id,
}: {
  value: number | string | null | undefined
  onChange: (value: number | string | null) => void
  options: ComboOption[]
  placeholder?: string
  allowClear?: boolean
  id?: string
}) {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [active, setActive] = useState(0)
  const ref = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  const listId = useId()
  useDismiss(ref, open, () => setOpen(false))

  const selected = options.find((o) => o.value === value) ?? null
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    return q ? options.filter((o) => o.label.toLowerCase().includes(q)) : options
  }, [options, query])

  useEffect(() => {
    if (open) inputRef.current?.focus()
    else setQuery('')
  }, [open])
  useEffect(() => setActive(0), [query, open])

  const choose = (o: ComboOption) => {
    onChange(o.value)
    setOpen(false)
  }
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      if (!open) setOpen(true)
      else setActive((a) => Math.min(a + 1, filtered.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setActive((a) => Math.max(a - 1, 0))
    } else if (e.key === 'Home') {
      e.preventDefault()
      setActive(0)
    } else if (e.key === 'End') {
      e.preventDefault()
      setActive(filtered.length - 1)
    } else if (e.key === 'Enter') {
      if (open && filtered[active]) {
        e.preventDefault()
        choose(filtered[active])
      }
    } else if (e.key === 'Escape') {
      if (open) {
        e.preventDefault()
        setOpen(false)
      }
    }
  }

  return (
    <div ref={ref} className="relative">
      <div
        className={cx(
          'flex items-center gap-1 rounded-md border border-input-border bg-input px-2 shadow-xs',
          'transition-[box-shadow,border-color] duration-fast ease-out',
          'focus-within:border-accent focus-within:ring-2 focus-within:ring-offset-2 focus-within:ring-offset-bg focus-within:shadow-focus',
        )}
        style={{ height: 'var(--control-h)' }}
      >
        <input
          ref={inputRef}
          id={id}
          role="combobox"
          aria-expanded={open}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={open && filtered[active] ? `${listId}-${active}` : undefined}
          className="min-w-0 flex-1 bg-transparent text-text outline-none placeholder:text-muted"
          placeholder={selected ? selected.label : placeholder}
          value={open ? query : selected?.label ?? ''}
          onChange={(e) => {
            setQuery(e.target.value)
            if (!open) setOpen(true)
          }}
          onFocus={() => setOpen(true)}
          onKeyDown={onKey}
        />
        {allowClear && selected && !open && (
          <button
            type="button"
            aria-label="Clear"
            className={cx('shrink-0 rounded-sm p-0.5 text-muted hover:text-text', focusRing)}
            onClick={() => onChange(null)}
          >
            <X size={14} />
          </button>
        )}
        <ChevronsUpDown size={14} className="shrink-0 text-muted" aria-hidden="true" />
      </div>
      {open && (
        <ul
          id={listId}
          role="listbox"
          className="absolute z-overlay mt-1 max-h-64 w-full overflow-auto rounded-md border border-border bg-surface py-1 shadow-overlay"
        >
          {filtered.length === 0 && <li className="t-caption px-3 py-2 text-muted">No matches</li>}
          {filtered.map((o, i) => (
            <li
              key={o.value}
              id={`${listId}-${i}`}
              role="option"
              aria-selected={o.value === value}
              onMouseEnter={() => setActive(i)}
              onMouseDown={(e) => {
                e.preventDefault()
                choose(o)
              }}
              className={cx(
                't-body cursor-pointer truncate border-l-2 px-3 py-1.5',
                i === active ? 'border-accent bg-accent-soft text-text' : 'border-transparent text-text',
                o.value === value && i !== active && 'text-accent',
              )}
            >
              {o.label}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
