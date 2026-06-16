// Built-in theme library (seed). Graphite is the base/default.
import { graphite } from './graphite'
import { editorial } from './editorial'
import { swiss } from './swiss'
import { humanist } from './humanist'
import { monotech } from './monotech'
import type { Theme } from '../contract'

export const builtinThemes: Theme[] = [graphite, editorial, swiss, humanist, monotech]
