// A tiny inline trend line — one normalized SVG path, optional area fill, no chart library. Degrades to
// a flat baseline for <2 points or a zero-range series (honest, not faked).

export function Sparkline({
  values,
  stroke = 'var(--viz-1)',
  fill,
  width = 96,
  height = 28,
  className,
}: {
  values: number[]
  stroke?: string
  fill?: string
  width?: number
  height?: number
  className?: string
}) {
  const pad = 2
  const min = values.length ? Math.min(...values) : 0
  const max = values.length ? Math.max(...values) : 0
  const span = max - min

  const baseline = (
    <line x1={0} y1={height / 2} x2={width} y2={height / 2} stroke={stroke} strokeWidth={1.5} opacity={0.5} vectorEffect="non-scaling-stroke" />
  )

  let content = baseline
  if (values.length >= 2 && span > 0) {
    const pts = values.map((v, i) => {
      const x = (i / (values.length - 1)) * width
      const y = pad + (1 - (v - min) / span) * (height - 2 * pad)
      return [x, y] as const
    })
    const d = pts.map(([x, y], i) => `${i === 0 ? 'M' : 'L'}${x.toFixed(1)},${y.toFixed(1)}`).join(' ')
    const area = fill ? `${d} L${width},${height} L0,${height} Z` : null
    content = (
      <>
        {area && <path d={area} fill={fill} opacity={0.12} />}
        <path d={d} fill="none" stroke={stroke} strokeWidth={1.5} strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" />
      </>
    )
  }

  return (
    <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" className={className} aria-hidden="true">
      {content}
    </svg>
  )
}
