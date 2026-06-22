// Overlay foundation shared by Dialog, Toast, CommandPalette, Combobox and Tooltip: a Portal, a focus
// trap with restore, an Escape/outside-click dismiss, and a reduced-motion hook. No dependencies beyond
// React + react-dom; everything reads the design tokens.

import { useEffect, useState, type ReactNode, type RefObject } from 'react'
import { createPortal } from 'react-dom'

/** Renders `children` into document.body so an overlay escapes ancestor stacking/overflow contexts. */
export function Portal({ children }: { children: ReactNode }) {
  const [el] = useState(() => document.createElement('div'))
  useEffect(() => {
    document.body.appendChild(el)
    return () => {
      document.body.removeChild(el)
    }
  }, [el])
  return createPortal(children, el)
}

/** True when the user prefers reduced motion — JS-driven enter animations check this and skip. */
export function useReducedMotion(): boolean {
  const [reduced, setReduced] = useState(
    () => typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches,
  )
  useEffect(() => {
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)')
    const on = () => setReduced(mq.matches)
    mq.addEventListener('change', on)
    return () => mq.removeEventListener('change', on)
  }, [])
  return reduced
}

const FOCUSABLE =
  'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])'

/** Traps Tab focus within `ref` while `active`, focuses the first control on open, restores to the
 * opener on close. The container itself should carry `tabIndex={-1}` as a focus fallback. */
export function useFocusTrap(ref: RefObject<HTMLElement>, active: boolean): void {
  useEffect(() => {
    const node = ref.current
    if (!active || !node) return
    const opener = document.activeElement as HTMLElement | null
    const items = () => Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter((el) => el.offsetParent !== null)
    ;(items()[0] ?? node).focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Tab') return
      const list = items()
      if (list.length === 0) {
        e.preventDefault()
        return
      }
      const idx = list.indexOf(document.activeElement as HTMLElement)
      if (e.shiftKey && idx <= 0) {
        e.preventDefault()
        list[list.length - 1].focus()
      } else if (!e.shiftKey && idx === list.length - 1) {
        e.preventDefault()
        list[0].focus()
      }
    }
    node.addEventListener('keydown', onKey)
    return () => {
      node.removeEventListener('keydown', onKey)
      opener?.focus?.()
    }
  }, [ref, active])
}

/** Calls `onDismiss` on Escape or a pointer-down outside `ref`. Pointer-down (not click) so a drag that
 * starts inside the panel and releases outside does not dismiss. */
export function useDismiss(ref: RefObject<HTMLElement>, active: boolean, onDismiss: () => void): void {
  useEffect(() => {
    if (!active) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onDismiss()
    }
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node | null
      // A nested popover (e.g. a Combobox option) selects on mousedown and removes itself; React 18 has
      // already flushed that re-render by the time this document handler runs, so the target is detached.
      // A detached target was inside our own subtree — never treat it as an outside click.
      if (!target || !target.isConnected) return
      if (ref.current && !ref.current.contains(target)) onDismiss()
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('mousedown', onDown)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onDown)
    }
  }, [ref, active, onDismiss])
}
