// Entry point for the claude.ai/design design-system sync (see .design-sync/config.json `entry`).
// Re-exports ONLY the standalone UI primitives — never the app shell (App/main/screens), whose
// top-level side effects (e.g. createRoot on #root) would crash the bundle's IIFE.
export { Dialog, confirm } from './ui/Dialog'
export { ToastProvider, useToast } from './ui/Toast'
export { Combobox } from './ui/Combobox'
export { CommandPalette } from './ui/CommandPalette'
export { Tooltip } from './ui/Tooltip'
export { Tabs } from './ui/Tabs'
export { Skeleton, SkeletonText, SkeletonTable, SkeletonStat } from './ui/Skeleton'
export { Sparkline } from './ui/Sparkline'
