import { useState, type FormEvent } from 'react'
import { LogIn } from 'lucide-react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import { Button, Card } from '../ui'

export function Login() {
  const { login } = useAuth()
  const [loginName, setLoginName] = useState('admin')
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  async function submit(e: FormEvent): Promise<void> {
    e.preventDefault()
    setError(null)
    setBusy(true)
    try {
      await login(loginName.trim(), password)
    } catch (err: unknown) {
      setError(err instanceof ApiError && err.status === 401 ? 'Invalid credentials.' : 'Sign-in failed.')
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="h-screen grid place-items-center bg-bg px-4">
      <div className="w-full max-w-sm">
        <div className="flex items-center gap-2.5 mb-6 justify-center">
          <div className="h-9 w-9 rounded-md bg-accent text-accent-fg grid place-items-center font-bold">
            M
          </div>
          <div className="leading-tight">
            <div className="t-h2 text-text">Meshble</div>
            <div className="text-[11px] text-muted">Sign in to continue</div>
          </div>
        </div>

        <Card className="p-6">
          <form onSubmit={submit} className="space-y-4">
            <label className="block">
              <span className="t-label text-muted">Login</span>
              <input
                value={loginName}
                onChange={(e) => setLoginName(e.target.value)}
                autoComplete="username"
                className="mt-1.5 w-full px-3 rounded-md bg-surface2 border border-border text-text focus:outline-none focus:ring-2 focus:ring-[var(--color-ring)]"
                style={{ height: 'var(--control-h)' }}
              />
            </label>
            <label className="block">
              <span className="t-label text-muted">Password</span>
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                autoComplete="current-password"
                className="mt-1.5 w-full px-3 rounded-md bg-surface2 border border-border text-text focus:outline-none focus:ring-2 focus:ring-[var(--color-ring)]"
                style={{ height: 'var(--control-h)' }}
              />
            </label>

            {error && <div className="t-body text-danger bg-danger-bg rounded-md px-3 py-2">{error}</div>}

            <Button variant="primary" className="w-full" icon={<LogIn size={16} />}>
              {busy ? 'Signing in…' : 'Sign in'}
            </Button>
          </form>
        </Card>
        <p className="t-caption text-muted text-center mt-4">Meshble · contract-driven UI</p>
      </div>
    </div>
  )
}
