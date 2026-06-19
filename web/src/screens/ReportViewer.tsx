// Renders one of a record's reports: the server HTML in a sandboxed iframe (no scripts, no network),
// with a Download PDF button that degrades gracefully when no rasterizer is configured (501).

import { useEffect, useState } from 'react'
import { Download } from 'lucide-react'
import * as api from '../api'
import { cx, Dialog, focusRing } from '../ui'

export function ReportViewer({
  model,
  id,
  report,
  onClose,
}: {
  model: string
  id: number
  report: api.ReportMeta
  onClose: () => void
}) {
  const [html, setHtml] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [pdfBusy, setPdfBusy] = useState(false)
  const [pdfNote, setPdfNote] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    void api
      .reportHtml(model, id, report.name)
      .then((h) => {
        if (!cancelled) setHtml(h)
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : 'Could not load the report')
      })
    return () => {
      cancelled = true
    }
  }, [model, id, report.name])

  async function downloadPdf(): Promise<void> {
    setPdfBusy(true)
    setPdfNote(null)
    try {
      const blob = await api.reportPdf(model, id, report.name)
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `${report.title.replace(/[^a-z0-9]+/gi, '_')}-${id}.pdf`
      document.body.appendChild(a)
      a.click()
      a.remove()
      URL.revokeObjectURL(url)
    } catch (e: unknown) {
      setPdfNote(
        e instanceof api.ApiError && e.status === 501
          ? "PDF rendering isn't configured on this server — the HTML version is shown."
          : 'Could not generate the PDF.',
      )
    } finally {
      setPdfBusy(false)
    }
  }

  return (
    <Dialog
      open
      onClose={onClose}
      title={report.title}
      size="lg"
      footer={
        <>
          {pdfNote && <span className="t-caption mr-auto text-warning">{pdfNote}</span>}
          <button
            onClick={() => void downloadPdf()}
            disabled={pdfBusy || html == null}
            className={cx(
              'inline-flex items-center gap-2 rounded-md border border-border bg-surface2 px-3 font-medium text-text hover:bg-surface disabled:opacity-50',
              focusRing,
            )}
            style={{ height: 'var(--control-h)' }}
          >
            <Download size={15} /> {pdfBusy ? 'Preparing…' : 'Download PDF'}
          </button>
        </>
      }
    >
      {error ? (
        <div className="t-body text-danger">{error}</div>
      ) : html == null ? (
        <div className="t-body py-10 text-center text-muted">Rendering…</div>
      ) : (
        <iframe title={report.title} sandbox="" srcDoc={html} className="h-[62vh] w-full rounded-md border border-border bg-white" />
      )}
    </Dialog>
  )
}
