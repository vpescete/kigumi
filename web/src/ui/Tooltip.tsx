// A lightweight tooltip — opens on hover/focus-within, positioned by side. Used mainly to label
// icon-only buttons (the trigger is described by the bubble via aria-describedby on the wrapper).

import { useId, useState, type ReactNode } from 'react'
import { cx } from './cx'

type Side = 'top' | 'bottom' | 'left' | 'right'

const POS: Record<Side, string> = {
  top: 'bottom-full left-1/2 -translate-x-1/2 mb-1.5',
  bottom: 'top-full left-1/2 -translate-x-1/2 mt-1.5',
  left: 'right-full top-1/2 -translate-y-1/2 mr-1.5',
  right: 'left-full top-1/2 -translate-y-1/2 ml-1.5',
}

export function Tooltip({ label, side = 'top', children }: { label: string; side?: Side; children: ReactNode }) {
  const [open, setOpen] = useState(false)
  const id = useId()
  return (
    <span
      className="relative inline-flex"
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      onFocus={() => setOpen(true)}
      onBlur={() => setOpen(false)}
      aria-describedby={open ? id : undefined}
    >
      {children}
      {open && (
        <span
          id={id}
          role="tooltip"
          className={cx(
            'pointer-events-none absolute z-tooltip whitespace-nowrap rounded-sm px-2 py-1',
            'bg-text text-bg t-caption shadow-overlay',
            POS[side],
          )}
        >
          {label}
        </span>
      )}
    </span>
  )
}
