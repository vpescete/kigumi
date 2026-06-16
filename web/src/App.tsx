import { NavLink, Route, Routes } from 'react-router-dom'
import {
  LayoutDashboard,
  Package,
  Search,
  ShoppingCart,
  Sun,
  Moon,
  Users,
} from 'lucide-react'
import { THEMES, useTheme } from './theme'
import { cx } from './ui'
import { Dashboard } from './screens/Dashboard'
import { Orders } from './screens/Orders'
import { OrderDetail } from './screens/OrderDetail'
import { Customers } from './screens/Customers'
import { Products } from './screens/Products'

const NAV = [
  { to: '/', label: 'Dashboard', icon: LayoutDashboard, end: true },
  { to: '/orders', label: 'Sales Orders', icon: ShoppingCart, end: false },
  { to: '/customers', label: 'Customers', icon: Users, end: false },
  { to: '/products', label: 'Products', icon: Package, end: false },
]

function Sidebar() {
  return (
    <aside className="w-[240px] shrink-0 h-full bg-bg border-r border-border flex flex-col">
      <div className="h-14 flex items-center gap-2.5 px-4 border-b border-border">
        <div className="h-7 w-7 rounded-md bg-accent text-accent-fg grid place-items-center font-bold text-sm">
          M
        </div>
        <div className="leading-tight">
          <div className="font-semibold text-text">Meshble</div>
          <div className="text-[11px] text-muted">Sales</div>
        </div>
      </div>

      <nav className="flex-1 p-3 space-y-0.5">
        <div className="px-2 py-1.5 text-[11px] font-semibold uppercase tracking-wider text-muted">
          Workspace
        </div>
        {NAV.map((n) => (
          <NavLink
            key={n.to}
            to={n.to}
            end={n.end}
            className={({ isActive }) =>
              cx(
                'flex items-center gap-2.5 px-2.5 rounded-md text-sm font-medium transition-colors',
                isActive
                  ? 'bg-surface2 text-text'
                  : 'text-muted hover:text-text hover:bg-surface2',
              )
            }
            style={{ height: 'var(--control-h)' }}
          >
            <n.icon size={17} strokeWidth={2} />
            {n.label}
          </NavLink>
        ))}
      </nav>

      <div className="p-3 border-t border-border">
        <div className="text-[11px] text-muted px-2">Navigable mockup · mock data</div>
      </div>
    </aside>
  )
}

function ThemeSwitcher() {
  const { theme, setTheme, mode, toggleMode } = useTheme()
  const active = THEMES.find((t) => t.id === theme)
  return (
    <div className="flex items-center gap-2">
      <div className="hidden md:flex items-center gap-1 p-1 rounded-md bg-surface2 border border-border">
        {THEMES.map((t) => (
          <button
            key={t.id}
            onClick={() => setTheme(t.id)}
            title={t.blurb}
            className={cx(
              'px-2.5 py-1 rounded-sm text-xs font-medium transition-colors',
              theme === t.id ? 'bg-surface text-text shadow-sm' : 'text-muted hover:text-text',
            )}
          >
            {t.name}
          </button>
        ))}
      </div>
      <button
        onClick={toggleMode}
        title={`Switch to ${mode === 'dark' ? 'light' : 'dark'} mode`}
        className="h-8 w-8 grid place-items-center rounded-md text-muted hover:text-text hover:bg-surface2 border border-border"
      >
        {mode === 'dark' ? <Sun size={16} /> : <Moon size={16} />}
      </button>
      <div className="hidden lg:block text-xs text-muted w-28 leading-tight">
        {active?.blurb}
      </div>
    </div>
  )
}

function Topbar() {
  return (
    <header className="h-14 shrink-0 flex items-center justify-between gap-4 px-5 border-b border-border bg-bg">
      <div className="relative w-full max-w-sm">
        <Search
          size={15}
          className="absolute left-2.5 top-1/2 -translate-y-1/2 text-muted pointer-events-none"
        />
        <input
          placeholder="Search orders, customers…"
          className="w-full pl-8 pr-3 rounded-md bg-surface2 border border-border text-text placeholder:text-muted focus:outline-none focus:ring-2 focus:ring-[var(--color-ring)] focus:border-transparent"
          style={{ height: 'var(--control-h)' }}
        />
      </div>
      <div className="flex items-center gap-3">
        <ThemeSwitcher />
        <div className="h-8 w-8 rounded-full bg-accent text-accent-fg grid place-items-center text-xs font-semibold">
          VP
        </div>
      </div>
    </header>
  )
}

export function App() {
  return (
    <div className="flex h-screen overflow-hidden">
      <Sidebar />
      <div className="flex-1 flex flex-col min-w-0">
        <Topbar />
        <main className="flex-1 overflow-auto">
          <div className="max-w-[1100px] mx-auto px-6 py-7">
            <Routes>
              <Route path="/" element={<Dashboard />} />
              <Route path="/orders" element={<Orders />} />
              <Route path="/orders/:id" element={<OrderDetail />} />
              <Route path="/customers" element={<Customers />} />
              <Route path="/products" element={<Products />} />
            </Routes>
          </div>
        </main>
      </div>
    </div>
  )
}
