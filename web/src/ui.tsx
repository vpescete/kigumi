import type { ReactNode } from 'react'
import { STATE_LABEL, type OrderState } from './data'
import { cx, focusRing } from './ui/cx'

// Re-export the overlay/feedback primitives so `import { ... } from './ui'` reaches everything.
export { cx, focusRing }
export { Portal, useFocusTrap, useDismiss, useReducedMotion } from './ui/overlay'
export { Skeleton, SkeletonText, SkeletonTable, SkeletonStat } from './ui/Skeleton'
export { Tooltip } from './ui/Tooltip'
export { Sparkline } from './ui/Sparkline'
export { Dialog, confirm } from './ui/Dialog'
export { ToastProvider, useToast } from './ui/Toast'
export { Combobox, type ComboOption } from './ui/Combobox'
export { CommandPalette, type CommandSection, type CommandItem } from './ui/CommandPalette'

/* ── Button ─────────────────────────────────────────────────────────────────── */
type BtnVariant = 'primary' | 'secondary' | 'ghost'
export function Button({
  children,
  variant = 'secondary',
  icon,
  onClick,
  className,
  disabled,
}: {
  children?: ReactNode
  variant?: BtnVariant
  icon?: ReactNode
  onClick?: () => void
  className?: string
  disabled?: boolean
}) {
  const base =
    'inline-flex items-center justify-center gap-2 rounded-md px-3 font-medium whitespace-nowrap ' +
    'transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-1 ' +
    'focus-visible:ring-offset-bg disabled:opacity-50'
  const variants: Record<BtnVariant, string> = {
    primary: 'bg-accent text-accent-fg hover:bg-accent-hover shadow-sm',
    secondary: 'bg-surface2 text-text border border-border hover:bg-surface',
    ghost: 'text-muted hover:text-text hover:bg-surface2',
  }
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={cx(base, variants[variant], className)}
      style={{ height: 'var(--control-h)' }}
    >
      {icon}
      {children}
    </button>
  )
}

/* ── Card ───────────────────────────────────────────────────────────────────── */
export function Card({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={cx('bg-surface border border-border rounded-lg shadow-sm', className)}>{children}</div>
  )
}

/* ── Badge ──────────────────────────────────────────────────────────────────── */
type Tone = 'neutral' | 'success' | 'warning' | 'danger' | 'accent'
export function Badge({ children, tone = 'neutral' }: { children: ReactNode; tone?: Tone }) {
  const tones: Record<Tone, string> = {
    neutral: 'bg-surface2 text-muted',
    success: 'bg-success-bg text-success',
    warning: 'bg-warning-bg text-warning',
    danger: 'bg-danger-bg text-danger',
    accent: 'bg-accent text-accent-fg',
  }
  return (
    <span
      className={cx(
        'inline-flex items-center gap-1.5 rounded-sm px-2 py-0.5 text-xs font-medium',
        tones[tone],
      )}
    >
      {children}
    </span>
  )
}

const STATE_TONE: Record<OrderState, Tone> = {
  draft: 'neutral',
  sent: 'warning',
  done: 'success',
  cancel: 'danger',
}
export function StateBadge({ state }: { state: OrderState }) {
  return (
    <Badge tone={STATE_TONE[state]}>
      <span className="h-1.5 w-1.5 rounded-full bg-current opacity-80" />
      {STATE_LABEL[state]}
    </Badge>
  )
}

/* ── Page header ────────────────────────────────────────────────────────────── */
export function PageHeader({
  title,
  subtitle,
  actions,
}: {
  title: string
  subtitle?: string
  actions?: ReactNode
}) {
  return (
    <div className="flex items-start justify-between gap-4 mb-6">
      <div>
        <h1 className="t-h1 text-text">{title}</h1>
        {subtitle && <p className="t-subtitle text-muted mt-1.5">{subtitle}</p>}
      </div>
      {actions && <div className="flex items-center gap-2 shrink-0">{actions}</div>}
    </div>
  )
}

/* ── DataTable (generic) ────────────────────────────────────────────────────── */
export type Column<T> = {
  header: string
  render: (row: T) => ReactNode
  align?: 'left' | 'right'
  mono?: boolean
  width?: string
}
export function DataTable<T>({
  columns,
  rows,
  onRowClick,
  rowKey,
}: {
  columns: Column<T>[]
  rows: T[]
  onRowClick?: (row: T) => void
  rowKey: (row: T) => string | number
}) {
  return (
    <Card className="overflow-x-auto">
      <table className="w-full border-collapse text-text">
        <thead>
          <tr className="border-b border-border">
            {columns.map((c, i) => (
              <th
                key={i}
                style={{ width: c.width }}
                className={cx(
                  't-label px-4 py-2.5 text-muted',
                  c.align === 'right' ? 'text-right' : 'text-left',
                )}
              >
                {c.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr
              key={rowKey(row)}
              onClick={onRowClick ? () => onRowClick(row) : undefined}
              tabIndex={onRowClick ? 0 : undefined}
              role={onRowClick ? 'button' : undefined}
              onKeyDown={
                onRowClick
                  ? (e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault()
                        onRowClick(row)
                      }
                    }
                  : undefined
              }
              className={cx(
                'border-b border-border last:border-0',
                onRowClick && 'cursor-pointer hover:bg-surface2 focus:outline-none focus-visible:bg-surface2',
              )}
              style={{ height: 'var(--density-row)' }}
            >
              {columns.map((c, i) => (
                <td
                  key={i}
                  className={cx(
                    'px-4',
                    c.align === 'right' ? 'text-right' : 'text-left',
                    c.mono ? 't-mono' : 't-body',
                  )}
                >
                  {c.render(row)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </Card>
  )
}

/* ── Loading / error states ─────────────────────────────────────────────────── */
export function Loading({ label = 'Loading…' }: { label?: string }) {
  return <div className="t-body text-muted py-16 text-center">{label}</div>
}
export function ErrorState({ message }: { message: string }) {
  return (
    <Card className="p-6">
      <div className="t-body text-danger">⚠ {message}</div>
    </Card>
  )
}

/* ── Stat tile (dashboard) ──────────────────────────────────────────────────── */
export function Stat({
  label,
  value,
  delta,
  icon,
}: {
  label: string
  value: string
  delta?: { dir: 'up' | 'down'; text: string }
  icon?: ReactNode
}) {
  return (
    <Card className="p-4">
      <div className="flex items-center justify-between">
        <span className="t-caption text-muted">{label}</span>
        {icon && <span className="text-muted">{icon}</span>}
      </div>
      <div className="mt-2 t-display text-text">{value}</div>
      {delta && (
        <div className={cx('mt-1.5 t-caption font-medium', delta.dir === 'up' ? 'text-success' : 'text-danger')}>
          {delta.dir === 'up' ? '▲' : '▼'} {delta.text}
        </div>
      )}
    </Card>
  )
}
