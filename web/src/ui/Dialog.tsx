// Modal dialog — portalled, focus-trapped, Escape/backdrop dismiss, reduced-motion aware. Plus an
// imperative confirm() that mounts a transient dialog and resolves a boolean (for destructive actions).
// Self-contained: depends only on the overlay foundation + tokens (never imports back from ui.tsx).

import { useEffect, useRef, type ReactNode } from 'react'
import { createRoot } from 'react-dom/client'
import { X } from 'lucide-react'
import { Portal, useDismiss, useFocusTrap, useReducedMotion } from './overlay'
import { cx, focusRing } from './cx'

type Size = 'sm' | 'md' | 'lg' | 'xl'
const WIDTH: Record<Size, string> = { sm: 'max-w-sm', md: 'max-w-lg', lg: 'max-w-2xl', xl: 'max-w-4xl' }

export function Dialog({
  open,
  onClose,
  title,
  children,
  footer,
  size = 'md',
}: {
  open: boolean
  onClose: () => void
  title?: string
  children?: ReactNode
  footer?: ReactNode
  size?: Size
}) {
  const ref = useRef<HTMLDivElement>(null)
  useFocusTrap(ref, open)
  useDismiss(ref, open, onClose)
  const reduced = useReducedMotion()

  useEffect(() => {
    if (!open) return
    const prev = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    return () => {
      document.body.style.overflow = prev
    }
  }, [open])

  if (!open) return null
  return (
    <Portal>
      <div className="fixed inset-0 z-overlay flex items-start justify-center overflow-y-auto bg-bg/70 p-4 backdrop-blur-sm sm:p-8">
        <div
          ref={ref}
          role="dialog"
          aria-modal="true"
          aria-label={title}
          tabIndex={-1}
          className={cx(
            'relative z-dialog w-full rounded-lg border border-border bg-surface shadow-overlay',
            WIDTH[size],
            !reduced && 'msh-dialog-in',
            focusRing,
          )}
        >
          {title && (
            <header className="flex items-center justify-between gap-3 border-b border-border px-5 py-3.5">
              <h2 className="t-h2 text-text">{title}</h2>
              <button onClick={onClose} aria-label="Close" className={cx('rounded-md p-1 text-muted hover:text-text', focusRing)}>
                <X size={18} />
              </button>
            </header>
          )}
          <div className="px-5 py-4">{children}</div>
          {footer && <footer className="flex items-center justify-end gap-2 border-t border-border px-5 py-3.5">{footer}</footer>}
        </div>
      </div>
    </Portal>
  )
}

/** Confirms an action via a transient modal. Resolves true on confirm, false on cancel/dismiss. */
export function confirm(opts: {
  title: string
  body?: ReactNode
  confirmLabel?: string
  cancelLabel?: string
  tone?: 'danger' | 'accent'
}): Promise<boolean> {
  return new Promise((resolve) => {
    const host = document.createElement('div')
    document.body.appendChild(host)
    const root = createRoot(host)
    const close = (val: boolean) => {
      root.unmount()
      host.remove()
      resolve(val)
    }
    const btn = 'inline-flex h-[var(--control-h)] items-center justify-center rounded-md px-3 font-medium ' + focusRing
    root.render(
      <Dialog
        open
        title={opts.title}
        onClose={() => close(false)}
        size="sm"
        footer={
          <>
            <button className={cx(btn, 'text-muted hover:text-text hover:bg-surface2')} onClick={() => close(false)}>
              {opts.cancelLabel ?? 'Cancel'}
            </button>
            <button
              className={cx(btn, opts.tone === 'danger' ? 'bg-danger text-bg hover:opacity-90' : 'bg-accent text-accent-fg hover:bg-accent-hover')}
              onClick={() => close(true)}
            >
              {opts.confirmLabel ?? 'Confirm'}
            </button>
          </>
        }
      >
        {opts.body && <p className="t-body text-muted">{opts.body}</p>}
      </Dialog>,
    )
  })
}
