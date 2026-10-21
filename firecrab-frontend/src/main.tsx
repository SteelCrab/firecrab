import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './styles/tokens.css'
import './styles/base.css'
import './index.css'
import './styles/frame.css'
import './styles/detail.css'
import './styles/vm.css'
import App from './App.tsx'
import { I18nProvider } from "./i18n.tsx"

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <I18nProvider>
      <App />
    </I18nProvider>
  </StrictMode>,
)
