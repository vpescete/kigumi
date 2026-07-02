import { Tooltip } from 'meshble-web'

// Tooltip is hover-driven (the label shows on hover), so a static card renders its trigger.
const triggerStyle: React.CSSProperties = {
  padding: '8px 14px',
  borderRadius: 'var(--radius-md)',
  border: '1px solid var(--color-border)',
  background: 'var(--color-surface)',
  color: 'var(--color-text)',
  fontFamily: 'var(--font-body)',
  cursor: 'pointer',
}

export const Default = () => (
  <div style={{ padding: 48, display: 'flex', gap: 16, alignItems: 'center' }}>
    <Tooltip label="Archive this record">
      <button style={triggerStyle}>Hover me</button>
    </Tooltip>
    <Tooltip label="Opens on the side" side="right">
      <button style={triggerStyle}>Side: right</button>
    </Tooltip>
  </div>
)
