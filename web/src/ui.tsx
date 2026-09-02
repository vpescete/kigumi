import type { CSSProperties, ReactNode } from 'react'
import { AlertTriangle, ArrowDown, ArrowUp } from 'lucide-react'
import { cx, focusRing, focusRingDanger } from './ui/cx'

// Re-export the overlay/feedback primitives so `import { ... } from './ui'` reaches everything.
export { cx, focusRing, focusRingDanger }
export { Portal, useFocusTrap, useDismiss, useReducedMotion } from './ui/overlay'
export { Skeleton, SkeletonText, SkeletonTable, SkeletonStat } from './ui/Skeleton'
export { Tooltip } from './ui/Tooltip'
export { Sparkline } from './ui/Sparkline'
export { Dialog, confirm } from './ui/Dialog'
export { ToastProvider, useToast } from './ui/Toast'
export { Combobox, type ComboOption } from './ui/Combobox'
export { CommandPalette, type CommandSection, type CommandItem } from './ui/CommandPalette'
export { Tabs, type Tab } from './ui/Tabs'
export { Menu, type MenuItem, type MenuGroup } from './ui/Menu'

/* ── Button ─────────────────────────────────────────────────────────────────── */
// 'primary' is kept as an alias of 'default' so existing call sites don't change.
type BtnVariant = 'default' | 'primary' | 'secondary' | 'outline' | 'ghost' | 'destructive'
type BtnSize = 'sm' | 'md' | 'lg' | 'icon'

const BTN_VARIANTS: Record<Exclude<BtnVariant, 'primary'>, string> = {
  default: 'bg-accent text-accent-fg shadow-xs hover:bg-accent-hover active:bg-accent',
  secondary: 'bg-surface2 text-text border border-border shadow-xs hover:bg-surface hover:border-input-border active:bg-surface2',
  outline: 'bg-transparent text-text border border-border hover:bg-surface2 hover:border-input-border active:bg-surface',
  ghost: 'bg-transparent text-muted hover:text-text hover:bg-surface2 active:bg-surface',
  destructive: 'bg-danger text-bg shadow-xs hover:opacity-90 active:opacity-100',
}
const BTN_SIZES: Record<BtnSize, { cls: string; style: CSSProperties }> = {
  sm: { cls: 'text-xs px-2.5 gap-1.5', style: { height: 'calc(var(--control-h) - 4px)' } },
  md: { cls: 'px-3', style: { height: 'var(--control-h)' } },
  lg: { cls: 'px-4 text-[14px]', style: { height: 'calc(var(--control-h) + 4px)' } },
  icon: { cls: 'p-0 aspect-square', style: { height: 'var(--control-h)', width: 'var(--control-h)' } },
}

export function Button({
  children,
  variant = 'secondary',
  size = 'md',
  icon,
  onClick,
  className,
  disabled,
  type = 'submit',
  title,
  ariaLabel,
}: {
  children?: ReactNode
  variant?: BtnVariant
  size?: BtnSize
  icon?: ReactNode
  onClick?: () => void
  className?: string
  disabled?: boolean
  type?: 'button' | 'submit'
  title?: string
  ariaLabel?: string
}) {
  const v = variant === 'primary' ? 'default' : variant
  const sz = BTN_SIZES[size]
  const base =
    'inline-flex items-center justify-center gap-2 rounded-md font-medium whitespace-nowrap select-none ' +
    'transition-[color,background-color,box-shadow,border-color,transform] duration-fast ease-out ' +
    'active:translate-y-px disabled:opacity-50 disabled:pointer-events-none ' +
    (v === 'destructive' ? focusRingDanger : focusRing)
  return (
    <button
      type={type}
      onClick={onClick}
      disabled={disabled}
      title={title}
      aria-label={ariaLabel}
      className={cx(base, sz.cls, BTN_VARIANTS[v], className)}
      style={sz.style}
    >
      {icon}
      {children}
    </button>
  )
}

/* ── Card ───────────────────────────────────────────────────────────────────── */
// `interactive` adds a hover lift, for cards that act as a button/link.
export function Card({
  children,
  className,
  interactive,
}: {
  children: ReactNode
  className?: string
  interactive?: boolean
}) {
  return (
    <div
      className={cx(
        'bg-surface border border-border rounded-lg shadow-sm',
        interactive && 'transition-shadow duration-base ease-out hover:shadow-md hover:border-input-border',
        className,
      )}
    >
      {children}
    </div>
  )
}

/* ── Badge ──────────────────────────────────────────────────────────────────── */
// Pill with a soft tinted fill and a hairline tinted border (the Untitled status-pill look).
type Tone = 'neutral' | 'success' | 'warning' | 'danger' | 'accent'
export function Badge({ children, tone = 'neutral' }: { children: ReactNode; tone?: Tone }) {
  const tones: Record<Tone, string> = {
    neutral: 'bg-surface2 text-muted border-border',
    success: 'bg-success-bg text-success border-success/30',
    warning: 'bg-warning-bg text-warning border-warning/30',
    danger: 'bg-danger-bg text-danger border-danger/30',
    accent: 'bg-accent-soft text-accent border-accent/30',
  }
  return (
    <span
      className={cx(
        'inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs font-medium',
        tones[tone],
      )}
    >
      {children}
    </span>
  )
}

// The sale-order state vocabulary, the one place StateBadge needs it.
export type OrderState = 'draft' | 'sent' | 'done' | 'cancel'
const STATE_LABEL: Record<OrderState, string> = {
  draft: 'Draft',
  sent: 'Quotation Sent',
  done: 'Confirmed',
  cancel: 'Cancelled',
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
      <span className="h-1.5 w-1.5 rounded-full bg-current" />
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
          <tr className="border-b border-border bg-surface2/40">
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
                onRowClick &&
                  'cursor-pointer transition-colors duration-fast hover:bg-surface2 focus:outline-none focus-visible:bg-surface2 focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-[var(--color-ring)]',
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
      <div className="t-body text-danger flex items-center gap-2">
        <AlertTriangle size={15} className="shrink-0" />
        {message}
      </div>
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
        <div className={cx('mt-1.5 t-caption font-medium inline-flex items-center gap-1', delta.dir === 'up' ? 'text-success' : 'text-danger')}>
          {delta.dir === 'up' ? <ArrowUp size={12} /> : <ArrowDown size={12} />} {delta.text}
        </div>
      )}
    </Card>
  )
}
