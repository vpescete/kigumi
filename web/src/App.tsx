import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react'
import { NavLink, Outlet, useLocation, useNavigate, useParams } from 'react-router-dom'
import {
  Box,
  ChevronDown,
  ChevronRight,
  Landmark,
  LayoutDashboard,
  LogOut,
  Menu,
  Moon,
  Package,
  Palette,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  Settings as SettingsIcon,
  ShoppingCart,
  Sun,
  X,
} from 'lucide-react'
import { useTheme } from './theme'
import { useAuth } from './auth'
import * as api from './api'
import { cx, focusRing, CommandPalette, Loading, Portal, type CommandSection } from './ui'
import { modelTitle } from './format'
import { groupModels, useModels, type NavGroup } from './nav'
import { Login } from './screens/Login'

const GROUP_ICON: Record<string, ReactNode> = {
  Sales: <ShoppingCart size={15} />,
  Inventory: <Package size={15} />,
  Accounting: <Landmark size={15} />,
  Settings: <SettingsIcon size={15} />,
  Other: <Box size={15} />,
}

function Brand({ collapsed }: { collapsed?: boolean }) {
  return (
    <NavLink
      to="/"
      className={cx('flex h-14 shrink-0 items-center gap-2.5 overflow-hidden border-b border-border', collapsed ? 'justify-center px-0' : 'px-4')}
    >
      <div className="grid h-7 w-7 shrink-0 place-items-center rounded-md bg-accent text-sm font-bold text-accent-fg">M</div>
      {!collapsed && (
        <div className="whitespace-nowrap leading-tight">
          <div className="font-semibold text-text">Meshble</div>
          <div className="t-mono text-[10px] text-muted">precision ERP</div>
        </div>
      )}
    </NavLink>
  )
}

/** A nav row with the signature cyan scanline on the active item. */
function NavItem({ to, label, end, indent, onNavigate }: { to: string; label: string; end?: boolean; indent?: boolean; onNavigate?: () => void }) {
  return (
    <NavLink
      to={to}
      end={end}
      onClick={onNavigate}
      className={({ isActive }) =>
        cx(
          't-body relative flex items-center gap-2.5 rounded-md pr-2.5 font-medium',
          indent ? 'pl-5' : 'pl-2.5',
          isActive ? 'bg-accent-soft text-accent' : 'text-muted hover:bg-surface2 hover:text-text',
          focusRing,
        )
      }
      style={{ height: 'var(--control-h)' }}
    >
      {({ isActive }) => (
        <>
          {isActive && <span className="absolute bottom-1 left-0 top-1 w-0.5 rounded-full bg-accent" aria-hidden="true" />}
          <span className="truncate">{label}</span>
        </>
      )}
    </NavLink>
  )
}

const OPEN_KEY = 'msh-nav-open'
function loadOpen(): Record<string, boolean> {
  try {
    return JSON.parse(localStorage.getItem(OPEN_KEY) ?? '{}') as Record<string, boolean>
  } catch {
    return {}
  }
}

function SidebarNav({ groups, onNavigate }: { groups: NavGroup[]; onNavigate?: () => void }) {
  const [open, setOpen] = useState<Record<string, boolean>>(loadOpen)
  const toggle = (label: string) => {
    setOpen((prev) => {
      const next = { ...prev, [label]: prev[label] === false ? true : false }
      localStorage.setItem(OPEN_KEY, JSON.stringify(next))
      return next
    })
  }
  return (
    <nav className="flex-1 space-y-1 overflow-y-auto p-3">
      <NavItem to="/" label="Dashboard" end onNavigate={onNavigate} />
      {groups.map((g) => {
        const isOpen = open[g.label] !== false
        return (
          <div key={g.label} className="pt-1.5">
            <button
              onClick={() => toggle(g.label)}
              className={cx('flex w-full items-center gap-2 rounded-md px-2 py-1 text-muted hover:text-text', focusRing)}
            >
              <span className="text-muted">{GROUP_ICON[g.label] ?? <Box size={15} />}</span>
              <span className="t-label flex-1 text-left">{g.label}</span>
              {isOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
            </button>
            {isOpen && (
              <div className="mt-0.5 space-y-0.5">
                {g.models.map((m) => (
                  <NavItem key={m} to={`/m/${m}`} label={modelTitle(m)} indent onNavigate={onNavigate} />
                ))}
              </div>
            )}
          </div>
        )
      })}
    </nav>
  )
}

/** Collapsed rail: icon-only nav. Each item reveals a hover flyout (the label, and a group's models)
 * to the right — so navigation stays reachable without expanding. */
function RailItem({ icon, active, label, to, models }: { icon: ReactNode; active: boolean; label: string; to?: string; models?: string[] }) {
  const iconCls = cx(
    'grid h-9 w-9 place-items-center rounded-md',
    active ? 'bg-accent-soft text-accent' : 'text-muted hover:bg-surface2 hover:text-text',
    focusRing,
  )
  return (
    <div className="group relative flex justify-center">
      {to ? (
        <NavLink to={to} end className={iconCls} aria-label={label}>
          {icon}
        </NavLink>
      ) : (
        <span className={iconCls} role="img" aria-label={label}>
          {icon}
        </span>
      )}
      {/* Hover flyout (pl-2 bridges the gap so moving onto it doesn't close it). */}
      <div className="invisible absolute left-full top-0 z-dialog pl-2 opacity-0 transition-opacity duration-fast group-hover:visible group-hover:opacity-100">
        <div className="min-w-[12rem] rounded-lg border border-border bg-surface p-1.5 shadow-overlay">
          <div className="t-label px-2 pb-1 pt-0.5 text-muted">{label}</div>
          {models
            ? models.map((m) => <NavItem key={m} to={`/m/${m}`} label={modelTitle(m)} />)
            : to && <NavItem to={to} end label={label} />}
        </div>
      </div>
    </div>
  )
}

function RailNav({ groups }: { groups: NavGroup[] }) {
  const { pathname } = useLocation()
  return (
    <nav className="flex-1 space-y-1 overflow-y-auto overflow-x-visible p-2">
      <RailItem to="/" label="Dashboard" icon={<LayoutDashboard size={18} />} active={pathname === '/'} />
      {groups.map((g) => (
        <RailItem
          key={g.label}
          label={g.label}
          icon={GROUP_ICON[g.label] ?? <Box size={18} />}
          models={g.models}
          active={g.models.some((m) => pathname.startsWith(`/m/${m}`))}
        />
      ))}
    </nav>
  )
}

function Sidebar({ groups, collapsed, onNavigate, className }: { groups: NavGroup[]; collapsed?: boolean; onNavigate?: () => void; className?: string }) {
  return (
    <aside
      className={cx(
        'flex h-full shrink-0 flex-col border-r border-border bg-bg transition-[width] duration-base ease-out',
        collapsed ? 'w-[60px]' : 'w-[244px]',
        className,
      )}
    >
      <Brand collapsed={collapsed} />
      {collapsed ? <RailNav groups={groups} /> : <SidebarNav groups={groups} onNavigate={onNavigate} />}
      <div className={cx('flex items-center border-t border-border py-2.5', collapsed ? 'justify-center px-0' : 'gap-2 px-4')}>
        <span className="h-1.5 w-1.5 rounded-full bg-success" aria-hidden="true" />
        {!collapsed && <span className="t-mono text-[10px] text-muted">live · contract-driven</span>}
      </div>
    </aside>
  )
}

function Breadcrumbs() {
  const { pathname } = useLocation()
  const params = useParams()
  const crumbs: { label: string; to?: string }[] = [{ label: 'Home', to: '/' }]
  if (pathname === '/') crumbs.push({ label: 'Dashboard' })
  else if (pathname.startsWith('/theme-studio')) crumbs.push({ label: 'Theme Studio' })
  else if (params.model) {
    crumbs.push({ label: modelTitle(params.model), to: `/m/${params.model}` })
    if (params.id) crumbs.push({ label: params.id === 'new' ? 'New' : `#${params.id}` })
  }
  return (
    <nav aria-label="Breadcrumb" className="flex min-w-0 items-center gap-1.5 overflow-hidden">
      {crumbs.map((c, i) => (
        <span key={i} className="flex min-w-0 items-center gap-1.5">
          {i > 0 && <span className="text-muted/60">/</span>}
          {c.to && i < crumbs.length - 1 ? (
            <NavLink to={c.to} className="t-body shrink-0 text-muted hover:text-text">
              {c.label}
            </NavLink>
          ) : (
            <span className="t-body truncate text-text">{c.label}</span>
          )}
        </span>
      ))}
    </nav>
  )
}

/** The account menu (top-right avatar): identity + the Appearance controls (theme picker, light/dark,
 * Theme Studio) — the ONLY place theme is changed — + sign out. */
function UserMenu() {
  const { identity, logout } = useAuth()
  const { theme, setTheme, mode, toggleMode, themes } = useTheme()
  const nav = useNavigate()
  const [open, setOpen] = useState(false)
  return (
    <div className="relative">
      <button
        onClick={() => setOpen((v) => !v)}
        aria-label="Account"
        aria-haspopup="menu"
        aria-expanded={open}
        className={cx('grid h-8 w-8 place-items-center rounded-full bg-accent text-xs font-semibold text-accent-fg', focusRing)}
      >
        {identity ? `#${identity.uid}` : '–'}
      </button>
      {open && (
        <>
          <div className="fixed inset-0 z-overlay" onClick={() => setOpen(false)} aria-hidden="true" />
          <div role="menu" className="absolute right-0 z-dialog mt-2 w-64 rounded-lg border border-border bg-surface p-1.5 shadow-overlay">
            <div className="px-2.5 py-1.5">
              <div className="t-body font-medium text-text">User #{identity?.uid}</div>
              <div className="t-caption truncate text-muted">{identity?.groups.join(', ') || 'no groups'}</div>
            </div>

            <div className="my-1 border-t border-border" />
            <div className="t-label px-2.5 pb-1 pt-1 text-muted">Appearance</div>
            <div className="grid grid-cols-2 gap-1 px-1.5 pb-1">
              {themes.map((t) => (
                <button
                  key={t.id}
                  role="menuitemradio"
                  aria-checked={theme === t.id}
                  onClick={() => setTheme(t.id)}
                  title={`${t.name}${t.author && t.author !== 'Meshble' ? ` · ${t.author}` : ''}`}
                  className={cx(
                    'truncate rounded-md border px-2 py-1 text-xs font-medium',
                    theme === t.id ? 'border-accent/40 bg-accent-soft text-accent' : 'border-border text-muted hover:bg-surface2 hover:text-text',
                    focusRing,
                  )}
                >
                  {t.name}
                </button>
              ))}
            </div>
            <button
              role="menuitem"
              onClick={toggleMode}
              className={cx('t-body flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-text hover:bg-surface2', focusRing)}
            >
              {mode === 'dark' ? <Sun size={15} className="text-muted" /> : <Moon size={15} className="text-muted" />}
              {mode === 'dark' ? 'Light mode' : 'Dark mode'}
            </button>
            <button
              role="menuitem"
              onClick={() => {
                setOpen(false)
                nav('/theme-studio')
              }}
              className={cx('t-body flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-text hover:bg-surface2', focusRing)}
            >
              <Palette size={15} className="text-muted" /> Theme Studio
            </button>

            <div className="my-1 border-t border-border" />
            <button
              role="menuitem"
              onClick={() => void logout()}
              className={cx('t-body flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-muted hover:bg-surface2 hover:text-text', focusRing)}
            >
              <LogOut size={15} /> Sign out
            </button>
          </div>
        </>
      )}
    </div>
  )
}

function Topbar({
  onOpenCommand,
  onOpenDrawer,
  onToggleSidebar,
  collapsed,
}: {
  onOpenCommand: () => void
  onOpenDrawer: () => void
  onToggleSidebar: () => void
  collapsed: boolean
}) {
  return (
    <header className="flex h-14 shrink-0 items-center justify-between gap-3 border-b border-border bg-bg px-4 md:px-5">
      <div className="flex min-w-0 flex-1 items-center gap-3">
        {/* Desktop: collapse/expand the fixed sidebar. */}
        <button
          onClick={onToggleSidebar}
          aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
          className={cx('hidden h-8 w-8 shrink-0 place-items-center rounded-md text-muted hover:bg-surface2 hover:text-text md:grid', focusRing)}
        >
          {collapsed ? <PanelLeftOpen size={18} /> : <PanelLeftClose size={18} />}
        </button>
        {/* Mobile: open the navigation drawer. */}
        <button
          onClick={onOpenDrawer}
          aria-label="Open navigation"
          className={cx('grid h-8 w-8 shrink-0 place-items-center rounded-md text-muted hover:bg-surface2 hover:text-text md:hidden', focusRing)}
        >
          <Menu size={18} />
        </button>
        <Breadcrumbs />
      </div>
      <div className="flex items-center gap-2 md:gap-3">
        <button
          onClick={onOpenCommand}
          className={cx(
            'hidden items-center gap-2 rounded-md border border-border bg-surface2 pl-2.5 pr-2 text-muted hover:text-text sm:flex',
            focusRing,
          )}
          style={{ height: 'var(--control-h)' }}
        >
          <span className="t-body">Search…</span>
          <kbd className="t-mono rounded-sm border border-border px-1.5 py-0.5 text-[10px]">⌘K</kbd>
        </button>
        <UserMenu />
      </div>
    </header>
  )
}

/** Mounts the command palette and wires the global ⌘K shortcut + the model/action/record sections. */
function useCommandPalette(models: string[]) {
  const nav = useNavigate()
  const { logout } = useAuth()
  const [open, setOpen] = useState(false)
  const [records, setRecords] = useState<CommandSection | null>(null)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        setOpen((v) => !v)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  // Debounced record search across the main business models when the query looks specific.
  const ANCHORS = useMemo(() => ['sale.order', 'res.partner', 'product.product'].filter((m) => models.includes(m)), [models])
  const onQuery = useCallback(
    (q: string) => {
      const query = q.trim()
      if (query.length < 2) {
        setRecords(null)
        return
      }
      window.clearTimeout((onQuery as unknown as { t?: number }).t)
      ;(onQuery as unknown as { t?: number }).t = window.setTimeout(async () => {
        try {
          const pages = await Promise.all(ANCHORS.map((m) => api.list(m, { limit: 6, order: '-id' }).then((p) => ({ m, p }))))
          const items = pages.flatMap(({ m, p }) =>
            p.data
              .map((r) => ({ r, label: (r.name as string) || (r.code as string) || `#${r.id}` }))
              .filter(({ label }) => label.toLowerCase().includes(query.toLowerCase()))
              .slice(0, 5)
              .map(({ r, label }) => ({
                id: `rec:${m}:${r.id}`,
                label,
                hint: modelTitle(m),
                run: () => nav(`/m/${m}/${r.id}`),
              })),
          )
          setRecords(items.length ? { title: 'Records', items } : null)
        } catch {
          setRecords(null)
        }
      }, 250) as unknown as number
    },
    [ANCHORS, nav],
  )

  const sections = useMemo<CommandSection[]>(() => {
    const goto: CommandSection = {
      title: 'Go to',
      items: [
        { id: 'go:dashboard', label: 'Dashboard', icon: <LayoutDashboard size={15} />, run: () => nav('/') },
        ...models.map((m) => ({ id: `go:${m}`, label: modelTitle(m), hint: m, run: () => nav(`/m/${m}`) })),
      ],
    }
    const create: CommandSection = {
      title: 'Create',
      items: models.map((m) => ({ id: `new:${m}`, label: `New ${modelTitle(m)}`, icon: <Plus size={15} />, run: () => nav(`/m/${m}/new`) })),
    }
    const actions: CommandSection = {
      title: 'Actions',
      items: [{ id: 'act:signout', label: 'Sign out', icon: <LogOut size={15} />, run: () => void logout() }],
    }
    return records ? [records, goto, create, actions] : [goto, create, actions]
  }, [models, records, nav, logout])

  const node = <CommandPalette open={open} onClose={() => setOpen(false)} sections={sections} onQuery={onQuery} />
  return { open: () => setOpen(true), node }
}

const COLLAPSE_KEY = 'msh-sidebar-collapsed'

export function App() {
  const { identity, loading } = useAuth()
  const models = useModels()
  const groups = useMemo(() => groupModels(models), [models])
  const [drawer, setDrawer] = useState(false)
  const [collapsed, setCollapsed] = useState(() => localStorage.getItem(COLLAPSE_KEY) === '1')
  const command = useCommandPalette(models)
  const { pathname } = useLocation()

  const toggleSidebar = useCallback(() => {
    setCollapsed((c) => {
      const next = !c
      localStorage.setItem(COLLAPSE_KEY, next ? '1' : '0')
      return next
    })
  }, [])

  // Close the mobile drawer on navigation.
  useEffect(() => setDrawer(false), [pathname])

  if (loading) {
    return (
      <div className="grid h-screen place-items-center bg-bg">
        <Loading />
      </div>
    )
  }
  if (!identity) return <Login />

  return (
    <div className="flex h-screen overflow-hidden">
      <Sidebar groups={groups} collapsed={collapsed} className="hidden md:flex" />
      {drawer && (
        <Portal>
          <div className="fixed inset-0 z-drawer bg-bg/70 backdrop-blur-sm md:hidden" onClick={() => setDrawer(false)} aria-hidden="true" />
          <div className="fixed inset-y-0 left-0 z-drawer md:hidden">
            <Sidebar groups={groups} onNavigate={() => setDrawer(false)} />
          </div>
          <button
            onClick={() => setDrawer(false)}
            aria-label="Close navigation"
            className={cx('fixed left-[252px] top-3 z-drawer grid h-8 w-8 place-items-center rounded-md bg-surface text-muted shadow-overlay md:hidden', focusRing)}
          >
            <X size={18} />
          </button>
        </Portal>
      )}
      <div className="flex min-w-0 flex-1 flex-col">
        <Topbar onOpenCommand={command.open} onOpenDrawer={() => setDrawer(true)} onToggleSidebar={toggleSidebar} collapsed={collapsed} />
        <main className="flex-1 overflow-auto">
          <div className="mx-auto max-w-[1180px] px-4 py-6 sm:px-6 sm:py-7">
            <Outlet />
          </div>
        </main>
      </div>
      {command.node}
    </div>
  )
}
