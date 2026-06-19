/** Joins class names, dropping falsy parts. Shared by ui.tsx and the ui/* primitives. */
export const cx = (...parts: (string | false | null | undefined)[]): string => parts.filter(Boolean).join(' ')

/** Shared focus-visible ring — apply to any interactive element the design system styles. */
export const focusRing =
  'focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-1 focus-visible:ring-offset-bg'
