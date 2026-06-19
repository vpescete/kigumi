/** Joins class names, dropping falsy parts. Shared by ui.tsx and the ui/* primitives. */
export const cx = (...parts: (string | false | null | undefined)[]): string => parts.filter(Boolean).join(' ')
