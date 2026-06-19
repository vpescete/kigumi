// Client-side domain evaluator — mirrors the server's portable AST (crates/meshble-core/src/domain.rs
// `to_json`) for the subset evaluable against a flat record: a field condition, and/or/not, and a const.
// Used to apply a field's invisible_when / readonly_when live as the user edits. Decimal/monetary values
// arrive as JSON strings, so numeric comparisons coerce with Number().

export type DomainNode =
  | { const: boolean }
  | { not: DomainNode }
  | { and: DomainNode[] }
  | { or: DomainNode[] }
  | { field: string; op: string; value?: unknown }

type Values = Record<string, unknown>

function asNumber(v: unknown): number | null {
  if (typeof v === 'number') return v
  if (typeof v === 'string' && v.trim() !== '' && !Number.isNaN(Number(v))) return Number(v)
  return null
}

function compare(op: string, left: unknown, right: unknown): boolean {
  switch (op) {
    case 'is null':
      return left == null || left === ''
    case 'is not null':
      return left != null && left !== ''
    case 'in':
      return Array.isArray(right) && right.some((r) => String(r) === String(left))
    case 'not in':
      return !(Array.isArray(right) && right.some((r) => String(r) === String(left)))
    case 'like':
      return String(left ?? '').includes(String(right ?? ''))
    case 'ilike':
      return String(left ?? '').toLowerCase().includes(String(right ?? '').toLowerCase())
  }
  // Equality / ordering: compare numerically when both coerce, else as strings.
  const ln = asNumber(left)
  const rn = asNumber(right)
  const [l, r]: [number | string, number | string] =
    ln !== null && rn !== null ? [ln, rn] : [String(left ?? ''), String(right ?? '')]
  switch (op) {
    case '=':
      return l === r
    case '!=':
      return l !== r
    case '<':
      return l < r
    case '<=':
      return l <= r
    case '>':
      return l > r
    case '>=':
      return l >= r
    default:
      return false
  }
}

/** Evaluates a domain AST against a record's current values. Unknown/missing shapes default to false
 * (so a field stays visible/editable). Dotted field paths (relations) are not evaluable on a flat
 * record and likewise default to false. */
export function evalDomain(node: unknown, values: Values): boolean {
  if (node == null || typeof node !== 'object') return false
  const n = node as Record<string, unknown>
  if ('const' in n) return Boolean(n.const)
  if ('not' in n) return !evalDomain(n.not, values)
  if ('and' in n && Array.isArray(n.and)) return n.and.every((x) => evalDomain(x, values))
  if ('or' in n && Array.isArray(n.or)) return n.or.some((x) => evalDomain(x, values))
  if ('field' in n && typeof n.field === 'string' && 'op' in n) {
    if (n.field.includes('.')) return false // relational path — not evaluable on a flat record
    return compare(String(n.op), values[n.field], (n as { value?: unknown }).value)
  }
  return false
}
