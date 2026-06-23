import React from 'react'
import ReactDOM from 'react-dom/client'
import { createBrowserRouter, RouterProvider } from 'react-router-dom'
import { ThemeProvider } from './theme'
import { AuthProvider } from './auth'
import { ToastProvider } from './ui'
import { getAllThemes, loadDropInThemes } from './theme/registry'
import { injectThemes } from './theme/css'
import { App } from './App'
import { Dashboard } from './screens/Dashboard'
import { ModelList } from './screens/ModelList'
import { ModelForm } from './screens/ModelForm'
import { ThemeStudio } from './screens/ThemeStudio'
import { Modules } from './screens/Modules'
import { Access } from './screens/Access'
import { Reports } from './screens/Reports'
import './index.css'
import './type.css'

// Inject every theme's variables and set the persisted theme/mode BEFORE first paint (no FOUC).
injectThemes(getAllThemes())
const savedTheme = localStorage.getItem('msh-theme')
const savedMode = localStorage.getItem('msh-mode')
document.documentElement.dataset.theme =
  savedTheme && getAllThemes().some((t) => t.id === savedTheme) ? savedTheme : 'graphite'
document.documentElement.dataset.mode = savedMode === 'light' ? 'light' : 'dark'

// App is the layout route (shell + auth gate + <Outlet/>); a DATA router (not BrowserRouter) is what
// lets a dirty form block in-app navigation via useBlocker.
const router = createBrowserRouter([
  {
    element: <App />,
    children: [
      { index: true, element: <Dashboard /> },
      { path: 'm/:model', element: <ModelList /> },
      { path: 'm/:model/:id', element: <ModelForm /> },
      { path: 'theme-studio', element: <ThemeStudio /> },
      { path: 'modules', element: <Modules /> },
      { path: 'access', element: <Access /> },
      { path: 'reports', element: <Reports /> },
    ],
  },
])

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <ThemeProvider>
      <AuthProvider>
        <ToastProvider>
          <RouterProvider router={router} />
        </ToastProvider>
      </AuthProvider>
    </ThemeProvider>
  </React.StrictMode>,
)

// Community drop-ins load after first paint; they appear in the switcher when ready.
void loadDropInThemes()
