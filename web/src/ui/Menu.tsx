// A dropdown menu: a trigger button + grouped items. Used to collapse a record's operations (service
// methods, wizards, reports) into one "Actions" overflow instead of a row of flat buttons.
import { useRef, useState, type ReactNode } from 'react'
import { ChevronDown } from 'lucide-react'
import { cx, focusRing } from './cx'
import { useDismiss } from './overlay'

export type MenuItem = {
  label: string
  icon?: ReactNode
  onSelect: () => void
  tone?: 'default' | 'danger'
  disabled?: boolean
}
export type MenuGroup = { label?: string; items: MenuItem[] }

export function Menu({
  label,
  icon,
  groups,
  disabled,
  align = 'right',
}: {
  label: string
  icon?: ReactNode
  groups: MenuGroup[]
  disabled?: boolean
  align?: 'left' | 'right'
}) {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)
  useDismiss(ref, open, () => setOpen(false))
  const visible = groups.map((g) => ({ ...g, items: g.items })).filter((g) => g.items.length > 0)

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        disabled={disabled}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className={cx(
          'inline-flex items-center justify-center gap-1.5 rounded-md px-3 font-medium whitespace-nowrap select-none',
          'bg-surface2 text-text border border-border shadow-xs hover:bg-surface hover:border-input-border',
          'transition-[color,background-color,box-shadow,border-color] duration-fast ease-out disabled:opacity-50 disabled:pointer-events-none',
          focusRing,
        )}
        style={{ height: 'var(--control-h)' }}
      >
        {icon}
        {label}
        <ChevronDown size={14} className={cx('transition-transform duration-fast', open && 'rotate-180')} />
      </button>
      {open && (
        <div
          role="menu"
          className={cx(
            'absolute z-dialog mt-1.5 min-w-[13rem] rounded-lg border border-border bg-surface p-1 shadow-overlay',
            align === 'right' ? 'right-0' : 'left-0',
          )}
        >
          {visible.map((g, gi) => (
            <div key={gi} className={gi > 0 ? 'mt-1 border-t border-border pt-1' : ''}>
              {g.label && <div className="t-label px-2.5 pb-1 pt-1 text-muted">{g.label}</div>}
              {g.items.map((it, ii) => (
                <button
                  key={ii}
                  type="button"
                  role="menuitem"
                  disabled={it.disabled}
                  onClick={() => {
                    setOpen(false)
                    it.onSelect()
                  }}
                  className={cx(
                    't-body flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left',
                    it.tone === 'danger' ? 'text-danger hover:bg-danger/10' : 'text-text hover:bg-surface2',
                    'disabled:opacity-50 disabled:pointer-events-none',
                    focusRing,
                  )}
                >
                  {it.icon && <span className="shrink-0 text-muted">{it.icon}</span>}
                  {it.label}
                </button>
              ))}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
