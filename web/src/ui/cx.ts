/** Joins class names, dropping falsy parts. Shared by ui.tsx and the ui/* primitives. */
export const cx = (...parts: (string | false | null | undefined)[]): string => parts.filter(Boolean).join(' ')

/** Shared focus-visible ring: a crisp accent ring (ringColor.DEFAULT = --color-ring), a gap offset on
 * the bg, and a soft accent halo (--shadow-focus). One source of truth for every styled control. */
export const focusRing =
  'focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:shadow-focus'

/** The same focus treatment with a danger-tinted halo, for destructive controls and invalid inputs. */
export const focusRingDanger =
  'focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:shadow-focus-danger'
