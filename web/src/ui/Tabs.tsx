// A simple tab strip (the form notebook). The active tab carries the cyan scanline underline.

import { useState, type ReactNode } from 'react'
import { cx, focusRing } from './cx'

export interface Tab {
  id: string
  label: string
  content: ReactNode
}

export function Tabs({ tabs }: { tabs: Tab[] }) {
  const [active, setActive] = useState(0)
  if (tabs.length === 0) return null
  const current = Math.min(active, tabs.length - 1)
  return (
    <div>
      <div role="tablist" className="flex gap-1 overflow-x-auto border-b border-border">
        {tabs.map((t, i) => (
          <button
            key={t.id}
            role="tab"
            aria-selected={i === current}
            onClick={() => setActive(i)}
            className={cx(
              'relative -mb-px whitespace-nowrap px-4 py-2.5 text-[14px] font-medium',
              i === current ? 'text-text' : 'text-muted hover:text-text',
              focusRing,
            )}
          >
            {t.label}
            {i === current && <span className="absolute inset-x-2 bottom-0 h-0.5 rounded-full bg-accent" aria-hidden="true" />}
          </button>
        ))}
      </div>
      <div className="pt-5">{tabs[current].content}</div>
    </div>
  )
}
