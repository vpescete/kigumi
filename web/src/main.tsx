import React from 'react'
import ReactDOM from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import { ThemeProvider } from './theme'
import { AuthProvider } from './auth'
import { getAllThemes, loadDropInThemes } from './theme/registry'
import { injectThemes } from './theme/css'
import { App } from './App'
import './index.css'
import './type.css'

// Inject every theme's variables and set the persisted theme/mode BEFORE first paint (no FOUC).
injectThemes(getAllThemes())
const savedTheme = localStorage.getItem('msh-theme')
const savedMode = localStorage.getItem('msh-mode')
document.documentElement.dataset.theme =
  savedTheme && getAllThemes().some((t) => t.id === savedTheme) ? savedTheme : 'graphite'
document.documentElement.dataset.mode = savedMode === 'light' ? 'light' : 'dark'

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <ThemeProvider>
      <AuthProvider>
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </AuthProvider>
    </ThemeProvider>
  </React.StrictMode>,
)

// Community drop-ins load after first paint; they appear in the switcher when ready.
void loadDropInThemes()
