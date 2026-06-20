// Modules admin page: list every linked module with its state + dependencies, and install/uninstall.
// Install/uninstall update the install ledger; the server router is built from the installed set at
// startup, so a restart is required to load/unload a module's models — surfaced as a notice.
import { useCallback, useEffect, useState } from 'react'
import { AlertTriangle, Check, Info, Plus, Trash2 } from 'lucide-react'
import * as api from '../api'
import { useAuth } from '../auth'
import { MODULES_CHANGED } from '../nav'
import { Badge, Button, Card, ErrorState, PageHeader, SkeletonText, useToast } from '../ui'

export function Modules() {
  const { identity } = useAuth()
  const toast = useToast()
  const isAdmin = identity?.groups.includes('admin') ?? false
  const [mods, setMods] = useState<api.ModuleInfo[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)

  const load = useCallback(async (): Promise<void> => {
    try {
      setMods(await api.modules())
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load modules')
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  // The server applies install/uninstall live; tell the nav to refetch its catalog.
  const applied = (): void => {
    window.dispatchEvent(new Event(MODULES_CHANGED))
  }

  async function install(name: string): Promise<void> {
    setBusy(name)
    try {
      const r = await api.installModule(name)
      toast.success(r.installed.length ? `Installed: ${r.installed.join(', ')}` : 'Already installed')
      applied()
      await load()
    } catch (err: unknown) {
      toast.error(err instanceof api.ApiError ? err.message : 'Install failed')
    } finally {
      setBusy(null)
    }
  }

  async function uninstall(name: string): Promise<void> {
    setBusy(name)
    try {
      const r = await api.uninstallModule(name)
      toast.success(`Uninstalled ${r.uninstalled}`)
      applied()
      await load()
    } catch (err: unknown) {
      toast.error(err instanceof api.ApiError ? err.message : 'Uninstall failed')
    } finally {
      setBusy(null)
    }
  }

  if (error) return <ErrorState message={error} />

  const installedCount = mods?.filter((m) => m.installed).length ?? 0

  return (
    <div>
      <PageHeader
        title="Modules"
        subtitle={mods ? `${installedCount} of ${mods.length} installed` : ' '}
      />

      <Card className="mb-5 flex items-start gap-3 p-4">
        <Info size={16} className="mt-0.5 shrink-0 text-accent" />
        <p className="t-caption text-muted">
          Install and uninstall apply <span className="text-text">live</span> — the server stays up and the module's
          models become available (or unavailable) immediately, no restart. Uninstall keeps the data. Module reference
          data (e.g. default accounts or stock locations) is seeded by <span className="t-mono">meshble migrate</span>.
        </p>
      </Card>

      {!mods ? (
        <Card className="p-5">
          <SkeletonText lines={5} />
        </Card>
      ) : (
        <div className="space-y-3">
          {mods.map((m) => (
            <Card key={m.name} className="flex items-center justify-between gap-4 p-4">
              <div className="min-w-0">
                <div className="flex items-center gap-2.5">
                  <span className="t-subtitle font-medium text-text">{m.name}</span>
                  <span className="t-mono text-[11px] text-muted">v{m.version}</span>
                  {m.installed ? (
                    <Badge tone="success">
                      <Check size={11} /> Installed
                    </Badge>
                  ) : (
                    <Badge tone="neutral">Available</Badge>
                  )}
                </div>
                {m.summary && <p className="t-caption mt-1 text-muted">{m.summary}</p>}
                {m.depends.length > 0 && (
                  <div className="mt-2 flex flex-wrap items-center gap-1.5">
                    <span className="t-caption text-muted">Depends:</span>
                    {m.depends.map((d) => (
                      <span key={d.name} className="t-mono rounded-sm border border-border bg-surface2 px-1.5 py-0.5 text-[11px] text-muted">
                        {d.name} {d.req}
                      </span>
                    ))}
                  </div>
                )}
              </div>
              {isAdmin && (
                <div className="shrink-0">
                  {m.installed ? (
                    m.name === 'base' ? (
                      <span className="t-caption text-muted">core</span>
                    ) : (
                      <Button variant="outline" icon={<Trash2 size={15} />} disabled={busy !== null} onClick={() => uninstall(m.name)}>
                        {busy === m.name ? 'Working…' : 'Uninstall'}
                      </Button>
                    )
                  ) : (
                    <Button variant="primary" icon={<Plus size={15} />} disabled={busy !== null} onClick={() => install(m.name)}>
                      {busy === m.name ? 'Working…' : 'Install'}
                    </Button>
                  )}
                </div>
              )}
            </Card>
          ))}
          {!isAdmin && (
            <p className="t-caption flex items-center gap-1.5 text-muted">
              <AlertTriangle size={13} /> Installing or uninstalling modules requires the admin group.
            </p>
          )}
        </div>
      )}
    </div>
  )
}
