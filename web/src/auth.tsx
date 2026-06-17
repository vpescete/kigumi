import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'
import * as api from './api'
import type { Identity } from './api'

interface AuthState {
  identity: Identity | null
  loading: boolean
  login: (login: string, password: string) => Promise<void>
  logout: () => Promise<void>
}

const AuthContext = createContext<AuthState | null>(null)

export function AuthProvider({ children }: { children: ReactNode }) {
  const [identity, setIdentity] = useState<Identity | null>(null)
  const [loading, setLoading] = useState(true)

  // Restore a session from a persisted token on first load.
  useEffect(() => {
    let active = true
    async function boot(): Promise<void> {
      if (api.isAuthenticated()) {
        try {
          const id = await api.me()
          if (active) setIdentity(id)
        } catch {
          // Token rejected (expired/revoked) — fall through to logged-out.
        }
      }
      if (active) setLoading(false)
    }
    void boot()
    return () => {
      active = false
    }
  }, [])

  async function login(loginName: string, password: string): Promise<void> {
    await api.login(loginName, password)
    setIdentity(await api.me())
  }

  async function logout(): Promise<void> {
    await api.logout()
    setIdentity(null)
  }

  return (
    <AuthContext.Provider value={{ identity, loading, login, logout }}>{children}</AuthContext.Provider>
  )
}

export function useAuth(): AuthState {
  const value = useContext(AuthContext)
  if (!value) throw new Error('useAuth must be used within an AuthProvider')
  return value
}
