// Toast notifications — a provider mounted once at app root + a useToast() hook. Bottom-right stack,
// auto-dismiss, dismissible, polite live region (errors announce assertively via role=alert).

import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from 'react'
import { Check, Info, TriangleAlert, X } from 'lucide-react'
import { Portal } from './overlay'
import { cx, focusRing } from './cx'

type Tone = 'success' | 'error' | 'info'
interface ToastItem {
  id: number
  tone: Tone
  message: string
}
interface ToastApi {
  success: (message: string) => void
  error: (message: string) => void
  info: (message: string) => void
}

const ToastCtx = createContext<ToastApi | null>(null)

export function useToast(): ToastApi {
  const api = useContext(ToastCtx)
  if (!api) throw new Error('useToast must be used within a ToastProvider')
  return api
}

const TONE_ICON: Record<Tone, ReactNode> = {
  success: <Check size={16} />,
  error: <TriangleAlert size={16} />,
  info: <Info size={16} />,
}
const TONE_COLOR: Record<Tone, string> = {
  success: 'text-success',
  error: 'text-danger',
  info: 'text-accent',
}

const DISMISS_MS = 4500

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<ToastItem[]>([])
  const seq = useRef(0)
  const remove = useCallback((id: number) => setItems((xs) => xs.filter((x) => x.id !== id)), [])
  const push = useCallback(
    (tone: Tone, message: string) => {
      const id = (seq.current += 1)
      setItems((xs) => [...xs.slice(-2), { id, tone, message }]) // keep at most 3 visible
      window.setTimeout(() => remove(id), DISMISS_MS)
    },
    [remove],
  )
  const api = useMemo<ToastApi>(
    () => ({ success: (m) => push('success', m), error: (m) => push('error', m), info: (m) => push('info', m) }),
    [push],
  )

  return (
    <ToastCtx.Provider value={api}>
      {children}
      <Portal>
        <div className="fixed bottom-4 right-4 z-toast flex w-[min(92vw,360px)] flex-col gap-2" role="region" aria-live="polite" aria-label="Notifications">
          {items.map((t) => (
            <div
              key={t.id}
              role={t.tone === 'error' ? 'alert' : undefined}
              className={cx('msh-toast-in flex items-start gap-2.5 rounded-md border border-border bg-surface px-3 py-2.5 shadow-overlay')}
            >
              <span className={cx('mt-0.5 shrink-0', TONE_COLOR[t.tone])}>{TONE_ICON[t.tone]}</span>
              <span className="t-body min-w-0 flex-1 break-words text-text">{t.message}</span>
              <button onClick={() => remove(t.id)} aria-label="Dismiss" className={cx('-mr-1 shrink-0 rounded-sm p-0.5 text-muted hover:text-text', focusRing)}>
                <X size={14} />
              </button>
            </div>
          ))}
        </div>
      </Portal>
    </ToastCtx.Provider>
  )
}
