// Loading skeletons — a shimmering placeholder block plus composites for text, tables and stat tiles.
// The shimmer (`.msh-skeleton` in index.css) is killed by the global reduced-motion rule.

import { cx } from './cx'

export function Skeleton({
  w,
  h = '1em',
  rounded = 'rounded-md',
  className,
}: {
  w?: string
  h?: string
  rounded?: string
  className?: string
}) {
  return <span className={cx('msh-skeleton block', rounded, className)} style={{ width: w, height: h }} aria-hidden="true" />
}

export function SkeletonText({ lines = 3 }: { lines?: number }) {
  return (
    <div className="space-y-2" aria-hidden="true">
      {Array.from({ length: lines }).map((_, i) => (
        <Skeleton key={i} w={i === lines - 1 ? '60%' : '100%'} h="0.85em" />
      ))}
    </div>
  )
}

export function SkeletonTable({ rows = 6, cols = 4 }: { rows?: number; cols?: number }) {
  return (
    <div className="bg-surface border border-border rounded-lg overflow-hidden" aria-hidden="true" aria-busy="true">
      <div className="flex gap-4 px-4 py-3 border-b border-border">
        {Array.from({ length: cols }).map((_, i) => (
          <Skeleton key={i} w="6rem" h="0.7em" />
        ))}
      </div>
      {Array.from({ length: rows }).map((_, r) => (
        <div key={r} className="flex gap-4 px-4 items-center border-b border-border last:border-0" style={{ height: 'var(--density-row)' }}>
          {Array.from({ length: cols }).map((_, c) => (
            <Skeleton key={c} w={c === 0 ? '40%' : '6rem'} h="0.8em" />
          ))}
        </div>
      ))}
    </div>
  )
}

export function SkeletonStat() {
  return (
    <div className="bg-surface border border-border rounded-lg p-4 space-y-3" aria-hidden="true" aria-busy="true">
      <Skeleton w="5rem" h="0.7em" />
      <Skeleton w="7rem" h="1.8em" />
      <Skeleton w="100%" h="1.6em" rounded="rounded-sm" />
    </div>
  )
}
