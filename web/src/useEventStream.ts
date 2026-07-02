// Live-refresh hook: subscribe to the record stream for one model and fire a (debounced) callback
// whenever any visible record of that model changes — the screen refetches through the normal
// secured reads (events are hints, never content).
import { useEffect, useRef } from 'react'
import * as api from './api'

export function useEventStream(model: string | undefined, onChange: () => void): void {
  const cb = useRef(onChange)
  cb.current = onChange
  useEffect(() => {
    if (!model) return
    let timer: number | undefined
    const stop = api.streamEvents([model], () => {
      window.clearTimeout(timer)
      timer = window.setTimeout(() => cb.current(), 300) // collapse bursts into one refetch
    })
    return () => {
      window.clearTimeout(timer)
      stop()
    }
  }, [model])
}
