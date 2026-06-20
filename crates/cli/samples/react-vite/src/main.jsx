import React from 'react'
import { createRoot } from 'react-dom/client'

const App = () => (
  <div style={{ fontFamily: 'sans-serif', padding: '2rem' }}>
    <h1>Hello from React/Vite</h1>
  </div>
)

createRoot(document.getElementById('root')).render(<App />)
