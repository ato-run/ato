import React, { useState, useEffect, useMemo, useRef } from 'react';
import {
  Search, Terminal, Play, Square, Globe, Package,
  Zap, Box, ChevronLeft, List, LayoutGrid, FolderPlus,
  CheckCircle2, Clock, Activity, Hash, X, FileText,
  Settings2, Trash2, RotateCcw, Layers, Database,
  AlertTriangle, Braces, Monitor, ExternalLink, Copy
} from 'lucide-react';

/* ─────────────────────────────────────────────
   FONTS (injected via style tag)
───────────────────────────────────────────── */
const FontStyle = () => (
  <style>{`
    @import url('https://fonts.googleapis.com/css2?family=DM+Mono:ital,wght@0,300;0,400;0,500;1,400&family=DM+Sans:wght@300;400;500;600&display=swap');

    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    
    :root {
      --bg:        #f7f7f8;
      --surface:   #ffffff;
      --border:    #e4e4e7;
      --border-md: #d1d1d6;
      --text-1:    #18181b;
      --text-2:    #52525b;
      --text-3:    #a1a1aa;
      --accent:    #2563eb;
      --accent-lt: #eff6ff;
      --accent-md: #bfdbfe;
      --green:     #16a34a;
      --green-lt:  #f0fdf4;
      --red:       #dc2626;
      --red-lt:    #fef2f2;
      --sidebar-bg:#18181b;
      --sidebar-bd:#27272a;
      --sidebar-tx:#a1a1aa;
      --sidebar-ax:#ffffff;
      --term-bg:   #18181b;
      --term-bd:   #27272a;
    }

    body, #root { height: 100vh; overflow: hidden; }

    .app {
      font-family: 'DM Sans', sans-serif;
      background: var(--bg);
      color: var(--text-1);
      height: 100vh;
      display: flex;
      overflow: hidden;
      font-size: 13px;
    }

    /* ── Sidebar ── */
    .sidebar {
      width: 220px;
      min-width: 220px;
      background: var(--sidebar-bg);
      border-right: 1px solid var(--sidebar-bd);
      display: flex;
      flex-direction: column;
      overflow: hidden;
    }

    .sidebar-logo {
      padding: 18px 20px 16px;
      border-bottom: 1px solid var(--sidebar-bd);
      display: flex;
      align-items: center;
      gap: 10px;
    }

    .logo-mark {
      width: 28px; height: 28px;
      background: var(--accent);
      border-radius: 6px;
      display: flex; align-items: center; justify-content: center;
      font-family: 'DM Mono', monospace;
      font-size: 13px; font-weight: 500;
      color: #fff;
      letter-spacing: -0.5px;
      flex-shrink: 0;
    }

    .logo-text {
      font-family: 'DM Mono', monospace;
      font-size: 11px;
      color: var(--sidebar-tx);
      letter-spacing: 0.08em;
      font-weight: 400;
    }

    .sidebar-section {
      padding: 12px 12px 4px;
    }

    .sidebar-label {
      font-family: 'DM Mono', monospace;
      font-size: 10px;
      color: #52525b;
      letter-spacing: 0.1em;
      text-transform: uppercase;
      padding: 0 8px;
      margin-bottom: 4px;
    }

    .nav-item {
      display: flex; align-items: center; gap: 9px;
      padding: 7px 10px;
      border-radius: 6px;
      font-size: 13px;
      color: var(--sidebar-tx);
      cursor: pointer;
      transition: background 0.12s, color 0.12s;
      border: none; background: none; width: 100%; text-align: left;
      font-family: 'DM Sans', sans-serif;
      font-weight: 400;
    }

    .nav-item:hover { background: #27272a; color: #d4d4d8; }
    .nav-item.active { background: #27272a; color: var(--sidebar-ax); }
    .nav-item .nav-icon { opacity: 0.7; flex-shrink: 0; }
    .nav-item.active .nav-icon { opacity: 1; }

    .nav-badge {
      margin-left: auto;
      background: var(--accent);
      color: #fff;
      font-family: 'DM Mono', monospace;
      font-size: 10px;
      font-weight: 500;
      padding: 1px 6px;
      border-radius: 10px;
    }

    /* Running sessions */
    .session-list {
      padding: 0 12px;
      display: flex; flex-direction: column; gap: 2px;
    }

    .session-item {
      display: flex; align-items: center; gap: 8px;
      padding: 6px 10px;
      border-radius: 6px;
      cursor: pointer;
      transition: background 0.12s;
      border: none; background: none; width: 100%; text-align: left;
    }

    .session-item:hover { background: #27272a; }

    .session-dot {
      width: 6px; height: 6px; border-radius: 50%;
      background: var(--green);
      flex-shrink: 0;
      animation: pulse 2s infinite;
    }

    @keyframes pulse {
      0%, 100% { opacity: 1; }
      50% { opacity: 0.4; }
    }

    .session-name {
      font-family: 'DM Mono', monospace;
      font-size: 11px;
      color: #a1a1aa;
      flex: 1;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .sidebar-footer {
      margin-top: auto;
      padding: 12px;
      border-top: 1px solid var(--sidebar-bd);
    }

    .target-chip {
      display: flex; align-items: center; gap: 8px;
      padding: 7px 10px;
      border-radius: 6px;
      background: #27272a;
      border: 1px solid var(--sidebar-bd);
    }

    .target-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--green); }

    .target-text {
      font-family: 'DM Mono', monospace;
      font-size: 11px;
      color: #71717a;
      flex: 1;
    }

    /* ── Main area ── */
    .main {
      flex: 1;
      display: flex;
      flex-direction: column;
      overflow: hidden;
    }

    .topbar {
      height: 52px;
      background: var(--surface);
      border-bottom: 1px solid var(--border);
      display: flex; align-items: center;
      padding: 0 24px;
      gap: 16px;
      flex-shrink: 0;
    }

    .page-title {
      font-size: 14px;
      font-weight: 600;
      color: var(--text-1);
      flex: 1;
    }

    .search-wrap {
      position: relative;
    }

    .search-wrap svg {
      position: absolute; left: 10px; top: 50%;
      transform: translateY(-50%);
      color: var(--text-3);
      pointer-events: none;
    }

    .search-input {
      padding: 6px 12px 6px 32px;
      border: 1px solid var(--border);
      border-radius: 6px;
      font-size: 12px;
      font-family: 'DM Sans', sans-serif;
      color: var(--text-1);
      background: var(--bg);
      width: 220px;
      outline: none;
      transition: border-color 0.15s, box-shadow 0.15s;
    }

    .search-input:focus {
      border-color: var(--accent);
      box-shadow: 0 0 0 3px rgba(37,99,235,0.08);
      background: var(--surface);
    }

    .btn {
      display: inline-flex; align-items: center; gap: 6px;
      padding: 6px 14px;
      border-radius: 6px;
      font-size: 12px;
      font-family: 'DM Sans', sans-serif;
      font-weight: 500;
      cursor: pointer;
      transition: all 0.12s;
      border: 1px solid transparent;
    }

    .btn:active { transform: translateY(1px); }

    .btn-primary {
      background: var(--accent);
      color: #fff;
    }

    .btn-primary:hover { background: #1d4ed8; }

    .btn-ghost {
      background: transparent;
      color: var(--text-2);
      border-color: var(--border);
    }

    .btn-ghost:hover { background: var(--bg); color: var(--text-1); }

    .btn-danger {
      background: var(--red-lt);
      color: var(--red);
      border-color: #fecaca;
    }

    .btn-danger:hover { background: var(--red); color: #fff; }

    .btn-success {
      background: var(--green-lt);
      color: var(--green);
      border-color: #bbf7d0;
    }

    .btn-success:hover { background: var(--green); color: #fff; }

    .icon-btn {
      display: inline-flex; align-items: center; justify-content: center;
      width: 28px; height: 28px;
      border-radius: 6px;
      border: 1px solid var(--border);
      background: var(--surface);
      color: var(--text-2);
      cursor: pointer;
      transition: all 0.12s;
    }

    .icon-btn:hover { background: var(--bg); color: var(--text-1); border-color: var(--border-md); }
    .icon-btn.active { background: var(--accent); color: #fff; border-color: var(--accent); }

    /* ── Toolbar ── */
    .toolbar {
      padding: 12px 24px;
      background: var(--surface);
      border-bottom: 1px solid var(--border);
      display: flex; align-items: center; gap: 8px;
      flex-shrink: 0;
    }

    .filter-group {
      display: flex;
      border: 1px solid var(--border);
      border-radius: 6px;
      overflow: hidden;
      background: var(--bg);
    }

    .filter-btn {
      padding: 5px 12px;
      font-size: 12px;
      font-family: 'DM Sans', sans-serif;
      font-weight: 400;
      color: var(--text-3);
      background: transparent;
      border: none;
      cursor: pointer;
      transition: all 0.12s;
      white-space: nowrap;
    }

    .filter-btn.active {
      background: var(--surface);
      color: var(--text-1);
      font-weight: 500;
      box-shadow: 0 1px 3px rgba(0,0,0,0.08);
    }

    .filter-btn:not(:last-child) { border-right: 1px solid var(--border); }

    .spacer { flex: 1; }

    .view-toggle { display: flex; gap: 4px; }

    /* ── Table ── */
    .content-scroll {
      flex: 1;
      overflow-y: auto;
      padding: 20px 24px;
    }

    table { width: 100%; border-collapse: collapse; }

    thead tr {
      border-bottom: 1px solid var(--border);
    }

    th {
      padding: 8px 12px;
      text-align: left;
      font-size: 11px;
      font-weight: 500;
      color: var(--text-3);
      font-family: 'DM Mono', monospace;
      letter-spacing: 0.04em;
      text-transform: uppercase;
    }

    th:last-child, td:last-child { text-align: right; }

    tbody tr {
      border-bottom: 1px solid var(--border);
      transition: background 0.1s;
      cursor: pointer;
    }

    tbody tr:last-child { border-bottom: none; }
    tbody tr:hover { background: var(--accent-lt); }

    td { padding: 12px 12px; vertical-align: middle; }

    .capsule-row {
      display: flex; align-items: center; gap: 12px;
    }

    .capsule-icon {
      width: 32px; height: 32px;
      border-radius: 8px;
      background: var(--bg);
      border: 1px solid var(--border);
      display: flex; align-items: center; justify-content: center;
      color: var(--text-2);
      flex-shrink: 0;
    }

    .capsule-name {
      font-family: 'DM Mono', monospace;
      font-size: 12px;
      font-weight: 500;
      color: var(--text-1);
    }

    .capsule-desc {
      font-size: 12px;
      color: var(--text-3);
      margin-top: 2px;
      max-width: 360px;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .mono-sm {
      font-family: 'DM Mono', monospace;
      font-size: 12px;
      color: var(--text-2);
    }

    .status-dot {
      width: 7px; height: 7px; border-radius: 50%;
      display: inline-block;
    }

    .status-dot.running { background: var(--green); animation: pulse 2s infinite; }
    .status-dot.stopped { background: var(--border-md); }

    .arch-badge {
      display: inline-flex;
      padding: 2px 6px;
      border-radius: 4px;
      font-family: 'DM Mono', monospace;
      font-size: 10px;
      font-weight: 500;
      border: 1px solid transparent;
    }

    .arch-badge.compat {
      background: var(--accent-lt);
      color: var(--accent);
      border-color: var(--accent-md);
    }

    .arch-badge.other {
      background: var(--bg);
      color: var(--text-3);
      border-color: var(--border);
    }

    .arch-wrap { display: flex; flex-wrap: wrap; gap: 4px; }

    .row-actions {
      display: flex; gap: 4px; justify-content: flex-end;
      opacity: 0;
      transition: opacity 0.12s;
    }

    tbody tr:hover .row-actions { opacity: 1; }

    /* Grid view */
    .grid-view {
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
      gap: 12px;
    }

    .grid-card {
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: 10px;
      padding: 16px;
      cursor: pointer;
      transition: all 0.15s;
    }

    .grid-card:hover {
      border-color: var(--accent-md);
      box-shadow: 0 4px 16px rgba(37,99,235,0.08);
    }

    .grid-card-header {
      display: flex; align-items: flex-start;
      justify-content: space-between;
      margin-bottom: 12px;
    }

    .grid-card-icon {
      width: 36px; height: 36px;
      border-radius: 9px;
      background: var(--bg);
      border: 1px solid var(--border);
      display: flex; align-items: center; justify-content: center;
      color: var(--text-2);
    }

    .grid-card-name {
      font-family: 'DM Mono', monospace;
      font-size: 12px;
      font-weight: 500;
      color: var(--text-1);
      margin-top: 10px;
      margin-bottom: 4px;
    }

    .grid-card-desc {
      font-size: 12px;
      color: var(--text-3);
      line-height: 1.4;
      display: -webkit-box;
      -webkit-line-clamp: 2;
      -webkit-box-orient: vertical;
      overflow: hidden;
    }

    .grid-card-footer {
      display: flex; align-items: center;
      justify-content: space-between;
      margin-top: 14px;
      padding-top: 12px;
      border-top: 1px solid var(--border);
    }

    /* Empty state */
    .empty-state {
      display: flex; flex-direction: column;
      align-items: center; justify-content: center;
      padding: 64px 24px;
      color: var(--text-3);
      gap: 12px;
    }

    .empty-state p {
      font-size: 13px;
      font-weight: 400;
    }

    /* ── Detail page ── */
    .detail-page {
      display: flex; flex-direction: column;
      height: 100%; overflow: hidden;
      background: var(--surface);
    }

    .detail-header {
      padding: 16px 24px;
      border-bottom: 1px solid var(--border);
      display: flex; align-items: center;
      gap: 16px;
      flex-shrink: 0;
      background: var(--surface);
    }

    .detail-icon {
      width: 40px; height: 40px;
      border-radius: 10px;
      background: var(--bg);
      border: 1px solid var(--border);
      display: flex; align-items: center; justify-content: center;
      color: var(--text-2);
      flex-shrink: 0;
    }

    .detail-title {
      font-family: 'DM Mono', monospace;
      font-size: 15px;
      font-weight: 500;
      color: var(--text-1);
    }

    .detail-meta {
      display: flex; align-items: center; gap: 12px;
      margin-top: 4px;
    }

    .detail-meta-item {
      font-size: 12px;
      color: var(--text-3);
      display: flex; align-items: center; gap: 4px;
    }

    .status-pill {
      display: inline-flex; align-items: center; gap: 5px;
      padding: 2px 8px;
      border-radius: 99px;
      font-size: 11px;
      font-weight: 500;
      border: 1px solid transparent;
    }

    .status-pill.running {
      background: var(--green-lt);
      color: var(--green);
      border-color: #bbf7d0;
    }

    .status-pill.stopped {
      background: var(--bg);
      color: var(--text-3);
      border-color: var(--border);
    }

    .detail-actions { margin-left: auto; display: flex; gap: 8px; }

    /* Tabs */
    .tabs {
      display: flex;
      border-bottom: 1px solid var(--border);
      padding: 0 24px;
      flex-shrink: 0;
      background: var(--surface);
    }

    .tab {
      display: flex; align-items: center; gap: 6px;
      padding: 10px 0;
      margin-right: 24px;
      font-size: 13px;
      font-weight: 400;
      color: var(--text-3);
      border-bottom: 2px solid transparent;
      cursor: pointer;
      transition: color 0.12s, border-color 0.12s;
      background: none; border-top: none; border-left: none; border-right: none;
    }

    .tab:hover { color: var(--text-1); }

    .tab.active {
      color: var(--accent);
      border-bottom-color: var(--accent);
      font-weight: 500;
    }

    /* Terminal */
    .terminal {
      flex: 1; overflow: hidden;
      display: flex; flex-direction: column;
      background: var(--term-bg);
    }

    .terminal-bar {
      display: flex; align-items: center;
      justify-content: space-between;
      padding: 8px 16px;
      background: #1c1c1e;
      border-bottom: 1px solid var(--term-bd);
      flex-shrink: 0;
    }

    .terminal-bar-title {
      font-family: 'DM Mono', monospace;
      font-size: 11px;
      color: #52525b;
    }

    .terminal-body {
      flex: 1; overflow-y: auto;
      padding: 16px;
      font-family: 'DM Mono', monospace;
      font-size: 12px;
      line-height: 1.7;
    }

    .log-line {
      display: flex; gap: 16px;
    }

    .log-num {
      color: #3f3f46;
      min-width: 28px;
      text-align: right;
      user-select: none;
      flex-shrink: 0;
    }

    .log-text { color: #d4d4d8; word-break: break-all; flex: 1; }
    .log-text.warn { color: #fbbf24; }
    .log-text.info { color: #60a5fa; }
    .log-text.error { color: #f87171; }

    .term-empty {
      height: 100%;
      display: flex; align-items: center; justify-content: center;
      font-family: 'DM Mono', monospace;
      font-size: 12px;
      color: #3f3f46;
    }

    /* Docs tab */
    .docs-pane {
      flex: 1; overflow-y: auto;
      padding: 32px;
      background: var(--bg);
    }

    .docs-card {
      max-width: 720px;
      margin: 0 auto;
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: 10px;
      padding: 32px;
    }

    .docs-card h1 {
      font-family: 'DM Mono', monospace;
      font-size: 18px;
      font-weight: 500;
      margin-bottom: 16px;
      color: var(--text-1);
    }

    .docs-card h2 {
      font-family: 'DM Sans', sans-serif;
      font-size: 14px;
      font-weight: 600;
      margin: 20px 0 8px;
      color: var(--text-1);
    }

    .docs-card p {
      font-size: 13px;
      color: var(--text-2);
      line-height: 1.65;
      margin-bottom: 12px;
    }

    .docs-card li {
      font-size: 13px;
      color: var(--text-2);
      line-height: 1.65;
      margin-left: 20px;
      margin-bottom: 4px;
    }

    .docs-card code {
      font-family: 'DM Mono', monospace;
      font-size: 11px;
      background: var(--bg);
      border: 1px solid var(--border);
      padding: 1px 5px;
      border-radius: 4px;
      color: var(--accent);
    }

    .docs-card pre {
      background: var(--term-bg);
      border: 1px solid var(--term-bd);
      border-radius: 8px;
      padding: 16px;
      margin: 12px 0;
      overflow-x: auto;
    }

    .docs-card pre code {
      background: none; border: none;
      padding: 0; color: #93c5fd;
      font-size: 12px; line-height: 1.7;
    }

    /* Config tab */
    .config-pane {
      flex: 1; overflow-y: auto;
      padding: 24px;
      background: var(--bg);
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 20px;
      align-content: start;
    }

    @media (max-width: 900px) {
      .config-pane { grid-template-columns: 1fr; }
    }

    .config-section {
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: 10px;
      overflow: hidden;
    }

    .config-section-header {
      display: flex; align-items: center; gap: 8px;
      padding: 12px 16px;
      border-bottom: 1px solid var(--border);
      font-size: 12px;
      font-weight: 500;
      color: var(--text-2);
    }

    .toml-body {
      background: var(--term-bg);
      padding: 16px;
      font-family: 'DM Mono', monospace;
      font-size: 12px;
      line-height: 1.7;
      overflow: auto;
      max-height: 380px;
    }

    .toml-line { display: flex; gap: 16px; }
    .toml-num { color: #3f3f46; min-width: 24px; text-align: right; user-select: none; }
    .toml-key { color: #93c5fd; }
    .toml-val { color: #86efac; }
    .toml-section { color: #fbbf24; }
    .toml-comment { color: #52525b; }

    .env-body { padding: 16px; display: flex; flex-direction: column; gap: 10px; }

    .env-row {
      display: flex; align-items: center; gap: 10px;
    }

    .env-key {
      font-family: 'DM Mono', monospace;
      font-size: 11px;
      color: var(--text-3);
      width: 130px;
      flex-shrink: 0;
      overflow: hidden;
      text-overflow: ellipsis;
    }

    .env-input {
      flex: 1;
      padding: 5px 10px;
      border: 1px solid var(--border);
      border-radius: 6px;
      font-family: 'DM Mono', monospace;
      font-size: 12px;
      color: var(--text-1);
      background: var(--bg);
      outline: none;
      transition: border-color 0.12s, box-shadow 0.12s;
    }

    .env-input:focus {
      border-color: var(--accent);
      box-shadow: 0 0 0 3px rgba(37,99,235,0.08);
      background: var(--surface);
    }

    /* Process Drawer */
    .drawer-overlay {
      position: fixed; inset: 0;
      background: rgba(0,0,0,0.3);
      z-index: 100;
      animation: fadeIn 0.15s ease;
    }

    .drawer {
      position: fixed; right: 0; top: 0; bottom: 0;
      width: 300px;
      background: var(--surface);
      border-left: 1px solid var(--border);
      z-index: 101;
      display: flex; flex-direction: column;
      box-shadow: -8px 0 32px rgba(0,0,0,0.1);
      animation: slideIn 0.2s ease;
    }

    @keyframes fadeIn { from { opacity: 0 } to { opacity: 1 } }
    @keyframes slideIn { from { transform: translateX(100%) } to { transform: translateX(0) } }

    .drawer-header {
      display: flex; align-items: center; justify-content: space-between;
      padding: 14px 16px;
      border-bottom: 1px solid var(--border);
    }

    .drawer-title {
      font-size: 13px;
      font-weight: 500;
      color: var(--text-1);
      display: flex; align-items: center; gap: 6px;
    }

    .drawer-body {
      flex: 1; overflow-y: auto;
      padding: 12px;
      display: flex; flex-direction: column; gap: 8px;
    }

    .proc-card {
      padding: 12px;
      border: 1px solid var(--border);
      border-radius: 8px;
      background: var(--surface);
      transition: border-color 0.12s, box-shadow 0.12s;
    }

    .proc-card:hover { border-color: var(--border-md); box-shadow: 0 2px 8px rgba(0,0,0,0.06); }

    .proc-header {
      display: flex; align-items: flex-start;
      justify-content: space-between;
      margin-bottom: 8px;
    }

    .proc-name {
      font-family: 'DM Mono', monospace;
      font-size: 12px;
      font-weight: 500;
      color: var(--text-1);
    }

    .proc-pid {
      font-family: 'DM Mono', monospace;
      font-size: 11px;
      color: var(--text-3);
      margin-top: 2px;
    }

    .proc-footer {
      display: flex; align-items: center;
      justify-content: space-between;
      padding-top: 8px;
      border-top: 1px solid var(--border);
      font-size: 11px;
      color: var(--text-3);
      font-family: 'DM Mono', monospace;
    }

    .proc-status {
      display: flex; align-items: center; gap: 5px;
      color: var(--green);
    }

    /* Toast */
    .toast {
      position: fixed; bottom: 20px; left: 50%;
      transform: translateX(-50%);
      z-index: 200;
      background: var(--text-1);
      color: #fff;
      padding: 9px 16px;
      border-radius: 8px;
      font-size: 12px;
      font-weight: 400;
      display: flex; align-items: center; gap: 8px;
      box-shadow: 0 4px 20px rgba(0,0,0,0.2);
      animation: toastIn 0.2s ease;
      white-space: nowrap;
    }

    @keyframes toastIn {
      from { opacity: 0; transform: translateX(-50%) translateY(8px); }
      to   { opacity: 1; transform: translateX(-50%) translateY(0); }
    }

    /* Scrollbars */
    ::-webkit-scrollbar { width: 6px; height: 6px; }
    ::-webkit-scrollbar-track { background: transparent; }
    ::-webkit-scrollbar-thumb { background: var(--border); border-radius: 3px; }
    ::-webkit-scrollbar-thumb:hover { background: var(--border-md); }

    /* Table wrapper card */
    .table-card {
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: 10px;
      overflow: hidden;
    }
  `}</style>
);

/* ─────────────────────────────────────────────
   MOCK DATA
───────────────────────────────────────────── */
const CAPSULES = [
  {
    id: "capsules-hello-web",
    scopedId: "capsules/hello-web",
    name: "hello-web",
    publisher: "capsules",
    iconKey: "globe",
    description: "Simple static web capsule for onboarding and launch checks.",
    appUrl: "http://localhost:3000",
    type: "webapp",
    version: "1.2.0",
    size: "2.8 MB",
    osArch: ["darwin/arm64", "linux/x64", "windows/x64"],
    envHints: { PORT: "3000", LOG_LEVEL: "info" },
    readme: `# hello-web\n\nLocal registry UI with a focus on functionality and performance.\n\n## Features\n\n- Low latency execution\n- Multi-platform support\n- Live log streaming with integrated terminal\n\n## Usage\n\n\`\`\`\nato run capsules/hello-web --registry http://localhost:8080\n\`\`\``,
    rawToml: `[capsule]\nname = "hello-web"\nversion = "1.2.0"\npublisher = "capsules"\n\n[runtime]\nos = ["darwin", "linux", "windows"]\narch = ["arm64", "x64"]\n\n[env]\nPORT = "3000"\nLOG_LEVEL = "info"`,
  },
  {
    id: "tools-json-linter",
    scopedId: "tools/json-linter",
    name: "json-linter",
    publisher: "tools",
    iconKey: "package",
    description: "CLI utility for high-performance JSON validation and formatting.",
    appUrl: null,
    type: "cli",
    version: "0.9.1",
    size: "9.4 MB",
    osArch: ["darwin/arm64", "linux/x64"],
    envHints: { STRICT: "true", MAX_SIZE: "10MB" },
    readme: `# json-linter\n\nOptimize your workflow with our advanced JSON linting engine.\n\n## Usage\n\n\`\`\`\nato run tools/json-linter -- --input data.json\n\`\`\``,
    rawToml: `[capsule]\nname = "json-linter"\nversion = "0.9.1"\npublisher = "tools"\n\n[runtime]\nos = ["darwin", "linux"]\narch = ["arm64", "x64"]\n\n[env]\nSTRICT = "true"\nMAX_SIZE = "10MB"`,
  },
  {
    id: "dev-fastapi-sample",
    scopedId: "dev/fastapi-sample",
    name: "fastapi-sample",
    publisher: "dev",
    iconKey: "zap",
    description: "Production-ready FastAPI backend scaffold with health checks.",
    appUrl: "http://localhost:8000",
    type: "webapp",
    version: "0.3.4",
    size: "14.2 MB",
    osArch: ["linux/x64"],
    envHints: { UVICORN_PORT: "8000", DEBUG: "false" },
    readme: `# fastapi-sample\n\nReady to deploy FastAPI application.\n\n## Usage\n\n\`\`\`\nato run dev/fastapi-sample\n\`\`\``,
    rawToml: `[capsule]\nname = "fastapi-sample"\nversion = "0.3.4"\npublisher = "dev"\n\n[runtime]\nos = ["linux"]\narch = ["x64"]\n\n[env]\nUVICORN_PORT = "8000"\nDEBUG = "false"`,
  },
];

const CURRENT_TARGET = "darwin/arm64";

/* ─────────────────────────────────────────────
   HELPERS
───────────────────────────────────────────── */
const CapsuleIcon = ({ iconKey, size = 16 }) => {
  const props = { size, strokeWidth: 1.5 };
  if (iconKey === 'globe') return <Globe {...props} />;
  if (iconKey === 'zap')   return <Zap {...props} />;
  return <Package {...props} />;
};

const logClass = (line) => {
  if (line.includes('WARN'))   return 'warn';
  if (line.includes('INFO'))   return 'info';
  if (line.includes('ERROR') || line.includes('SIGTERM')) return 'error';
  return '';
};

/* Minimal markdown-ish renderer */
const ReadmeRenderer = ({ text }) => {
  const lines = text.split('\n');
  const nodes = [];
  let i = 0;
  while (i < lines.length) {
    const l = lines[i];
    if (l.startsWith('# '))       { nodes.push(<h1 key={i}>{l.slice(2)}</h1>); i++; continue; }
    if (l.startsWith('## '))      { nodes.push(<h2 key={i}>{l.slice(3)}</h2>); i++; continue; }
    if (l.startsWith('- '))       { nodes.push(<li key={i}>{l.slice(2)}</li>); i++; continue; }
    if (l.startsWith('```')) {
      const code = [];
      i++;
      while (i < lines.length && !lines[i].startsWith('```')) { code.push(lines[i]); i++; }
      nodes.push(<pre key={i}><code>{code.join('\n')}</code></pre>);
      i++; continue;
    }
    if (l.trim() === '') { nodes.push(<br key={i} />); i++; continue; }
    nodes.push(<p key={i}>{l}</p>);
    i++;
  }
  return <div className="docs-card">{nodes}</div>;
};

/* TOML syntax highlighter */
const TomlRenderer = ({ text }) => (
  <div className="toml-body">
    {text.split('\n').map((line, i) => {
      let content;
      if (line.startsWith('#'))         content = <span className="toml-comment">{line}</span>;
      else if (line.startsWith('['))    content = <span className="toml-section">{line}</span>;
      else if (line.includes(' = ')) {
        const [k, ...v] = line.split(' = ');
        content = <><span className="toml-key">{k}</span><span style={{color:'#d4d4d8'}}> = </span><span className="toml-val">{v.join(' = ')}</span></>;
      } else                            content = <span style={{color:'#d4d4d8'}}>{line}</span>;
      return (
        <div key={i} className="toml-line">
          <span className="toml-num">{i + 1}</span>
          <span style={{flex:1}}>{content || ' '}</span>
        </div>
      );
    })}
  </div>
);

/* ─────────────────────────────────────────────
   SIDEBAR
───────────────────────────────────────────── */
const Sidebar = ({ view, setView, activeSessions, onSessionClick, onOpenDrawer }) => (
  <aside className="sidebar">
    <div className="sidebar-logo">
      <div className="logo-mark">ato</div>
      <span className="logo-text">local registry</span>
    </div>

    <div className="sidebar-section">
      <div className="sidebar-label">Navigate</div>
      <button
        className={`nav-item ${view === 'catalog' ? 'active' : ''}`}
        onClick={() => setView('catalog')}
        aria-label="Go to Library"
      >
        <Layers size={14} strokeWidth={1.5} className="nav-icon" />
        Library
      </button>
      <button
        className="nav-item"
        aria-label="Open store (coming soon)"
        onClick={() => {}}
      >
        <Database size={14} strokeWidth={1.5} className="nav-icon" />
        Store
        <span style={{marginLeft:'auto',fontSize:10,color:'#3f3f46'}}>soon</span>
      </button>
    </div>

    {activeSessions.length > 0 && (
      <div className="sidebar-section" style={{marginTop: 8}}>
        <div className="sidebar-label">Running</div>
        <div className="session-list">
          {activeSessions.map(s => (
            <button
              key={s.capsuleId}
              className="session-item"
              onClick={() => onSessionClick(s.capsuleId)}
              aria-label={`View ${s.scopedId}`}
            >
              <span className="session-dot" />
              <span className="session-name">{s.scopedId}</span>
            </button>
          ))}
        </div>
        <div style={{padding:'4px 12px 0'}}>
          <button className="nav-item" onClick={onOpenDrawer} aria-label="Open process manager">
            <Activity size={14} strokeWidth={1.5} className="nav-icon" />
            Processes
            <span className="nav-badge">{activeSessions.length}</span>
          </button>
        </div>
      </div>
    )}

    <div className="sidebar-footer">
      <div className="target-chip">
        <span className="target-dot" />
        <span className="target-text">{CURRENT_TARGET}</span>
      </div>
    </div>
  </aside>
);

/* ─────────────────────────────────────────────
   CATALOG PAGE
───────────────────────────────────────────── */
const CatalogPage = ({
  capsules, searchQuery, setSearchQuery,
  targetFilter, setTargetFilter,
  viewMode, setViewMode,
  activeProcesses, onStart, onInspect,
}) => (
  <div className="main">
    {/* Topbar */}
    <div className="topbar">
      <span className="page-title">Library</span>
      <div className="search-wrap">
        <Search size={13} strokeWidth={1.5} />
        <input
          className="search-input"
          type="text"
          placeholder="Search capsules…"
          value={searchQuery}
          onChange={e => setSearchQuery(e.target.value)}
          aria-label="Search capsules"
        />
      </div>
      <button className="btn btn-primary" aria-label="Publish a capsule">
        <FolderPlus size={13} strokeWidth={1.5} />
        Publish
      </button>
    </div>

    {/* Toolbar */}
    <div className="toolbar">
      <div className="filter-group" role="group" aria-label="Target filter">
        <button
          className={`filter-btn ${targetFilter === 'all' ? 'active' : ''}`}
          onClick={() => setTargetFilter('all')}
        >All</button>
        <button
          className={`filter-btn ${targetFilter === 'current' ? 'active' : ''}`}
          onClick={() => setTargetFilter('current')}
        >{CURRENT_TARGET}</button>
        <button
          className={`filter-btn ${targetFilter === 'cross' ? 'active' : ''}`}
          onClick={() => setTargetFilter('cross')}
        >Cross-platform</button>
      </div>

      <div className="spacer" />

      <span style={{fontSize:12,color:'var(--text-3)',marginRight:4}}>
        {capsules.length} capsule{capsules.length !== 1 ? 's' : ''}
      </span>

      <div className="view-toggle">
        <button
          className={`icon-btn ${viewMode === 'list' ? 'active' : ''}`}
          onClick={() => setViewMode('list')}
          aria-label="List view"
          aria-pressed={viewMode === 'list'}
        ><List size={14} strokeWidth={1.5} /></button>
        <button
          className={`icon-btn ${viewMode === 'grid' ? 'active' : ''}`}
          onClick={() => setViewMode('grid')}
          aria-label="Grid view"
          aria-pressed={viewMode === 'grid'}
        ><LayoutGrid size={14} strokeWidth={1.5} /></button>
      </div>
    </div>

    {/* Content */}
    <div className="content-scroll">
      {capsules.length === 0 ? (
        <div className="empty-state" role="status">
          <Search size={32} strokeWidth={1} color="var(--border-md)" />
          <p>No capsules found{searchQuery ? ` for "${searchQuery}"` : ''}.</p>
        </div>
      ) : viewMode === 'list' ? (
        <div className="table-card">
          <table>
            <thead>
              <tr>
                <th style={{width:32,paddingLeft:16}} aria-label="Status"></th>
                <th>Capsule</th>
                <th>Version</th>
                <th>Platforms</th>
                <th>Size</th>
                <th style={{paddingRight:16}}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {capsules.map(cap => {
                const running = Object.values(activeProcesses).some(p => p.capsuleId === cap.id);
                return (
                  <tr
                    key={cap.id}
                    onClick={() => onInspect(cap.id)}
                    role="button"
                    tabIndex={0}
                    onKeyDown={e => e.key === 'Enter' && onInspect(cap.id)}
                    aria-label={`Inspect ${cap.scopedId}`}
                  >
                    <td style={{paddingLeft:16}}>
                      <span
                        className={`status-dot ${running ? 'running' : 'stopped'}`}
                        aria-label={running ? 'Running' : 'Stopped'}
                      />
                    </td>
                    <td>
                      <div className="capsule-row">
                        <div className="capsule-icon">
                          <CapsuleIcon iconKey={cap.iconKey} size={15} />
                        </div>
                        <div>
                          <div className="capsule-name">{cap.scopedId}</div>
                          <div className="capsule-desc">{cap.description}</div>
                        </div>
                      </div>
                    </td>
                    <td><span className="mono-sm">v{cap.version}</span></td>
                    <td>
                      <div className="arch-wrap">
                        {cap.osArch.map(a => (
                          <span
                            key={a}
                            className={`arch-badge ${a === CURRENT_TARGET ? 'compat' : 'other'}`}
                          >{a}</span>
                        ))}
                      </div>
                    </td>
                    <td><span className="mono-sm">{cap.size}</span></td>
                    <td style={{paddingRight:16}}>
                      <div className="row-actions">
                        <button
                          className="btn btn-success"
                          style={{padding:'4px 10px'}}
                          onClick={e => { e.stopPropagation(); onStart(cap); }}
                          aria-label={`Run ${cap.scopedId}`}
                        >
                          <Play size={12} strokeWidth={2} fill="currentColor" />
                          Run
                        </button>
                        <button
                          className="btn btn-ghost"
                          style={{padding:'4px 10px'}}
                          onClick={e => { e.stopPropagation(); onInspect(cap.id); }}
                          aria-label={`Inspect ${cap.scopedId}`}
                        >
                          <Terminal size={12} strokeWidth={1.5} />
                        </button>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="grid-view">
          {capsules.map(cap => {
            const running = Object.values(activeProcesses).some(p => p.capsuleId === cap.id);
            return (
              <div
                key={cap.id}
                className="grid-card"
                onClick={() => onInspect(cap.id)}
                role="button"
                tabIndex={0}
                onKeyDown={e => e.key === 'Enter' && onInspect(cap.id)}
                aria-label={`Inspect ${cap.scopedId}`}
              >
                <div className="grid-card-header">
                  <div className="grid-card-icon">
                    <CapsuleIcon iconKey={cap.iconKey} size={16} />
                  </div>
                  <span
                    className={`status-pill ${running ? 'running' : 'stopped'}`}
                    aria-label={running ? 'Running' : 'Stopped'}
                  >
                    <span className="status-dot" style={{
                      width:5,height:5,background:'currentColor',
                      animation: running ? 'pulse 2s infinite' : 'none'
                    }} />
                    {running ? 'Running' : 'Idle'}
                  </span>
                </div>
                <div className="grid-card-name">{cap.scopedId}</div>
                <div className="grid-card-desc">{cap.description}</div>
                <div className="grid-card-footer">
                  <span className="mono-sm" style={{fontSize:11}}>v{cap.version}</span>
                  <button
                    className="btn btn-success"
                    style={{padding:'3px 10px', fontSize:11}}
                    onClick={e => { e.stopPropagation(); onStart(cap); }}
                    aria-label={`Run ${cap.scopedId}`}
                  >
                    <Play size={11} strokeWidth={2} fill="currentColor" /> Run
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  </div>
);

/* ─────────────────────────────────────────────
   DETAIL PAGE
───────────────────────────────────────────── */
const DetailPage = ({ capsule, onBack, onStart, activeProcs, onStop, logs, onClearLogs, overrides, onEnvUpdate }) => {
  const [tab, setTab] = useState('logs');
  const scrollRef = useRef(null);
  const isRunning = activeProcs.length > 0;

  useEffect(() => {
    if (scrollRef.current && tab === 'logs') {
      const el = scrollRef.current;
      if (el.scrollHeight - el.scrollTop - el.clientHeight < 80) {
        el.scrollTop = el.scrollHeight;
      }
    }
  }, [logs, tab]);

  return (
    <div className="main">
      <div className="detail-page">
        {/* Header */}
        <div className="detail-header">
          <button className="icon-btn" onClick={onBack} aria-label="Back to library">
            <ChevronLeft size={15} strokeWidth={1.5} />
          </button>
          <div className="detail-icon">
            <CapsuleIcon iconKey={capsule.iconKey} size={18} />
          </div>
          <div>
            <div style={{display:'flex', alignItems:'center', gap:10}}>
              <span className="detail-title">{capsule.scopedId}</span>
              <span className={`status-pill ${isRunning ? 'running' : 'stopped'}`}>
                <span style={{
                  width:5, height:5, borderRadius:'50%',
                  background:'currentColor',
                  animation: isRunning ? 'pulse 2s infinite' : 'none'
                }} />
                {isRunning ? `Running · ${activeProcs.length} instance${activeProcs.length > 1 ? 's' : ''}` : 'Idle'}
              </span>
            </div>
            <div className="detail-meta">
              <span className="detail-meta-item">
                <Hash size={11} strokeWidth={1.5} /> v{capsule.version}
              </span>
              <span style={{color:'var(--border-md)'}}>·</span>
              <span className="detail-meta-item">{capsule.publisher}</span>
              <span style={{color:'var(--border-md)'}}>·</span>
              <span className="detail-meta-item">{capsule.size}</span>
            </div>
          </div>
          <div className="detail-actions">
            {isRunning && (
              <button
                className="btn btn-danger"
                onClick={() => onStop(activeProcs[0].pid)}
                aria-label="Stop running instance"
              >
                <Square size={12} strokeWidth={2} fill="currentColor" /> Stop
              </button>
            )}
            <button
              className="btn btn-success"
              onClick={() => onStart(capsule)}
              aria-label="Spawn new instance"
            >
              <Play size={12} strokeWidth={2} fill="currentColor" />
              {isRunning ? 'Spawn another' : 'Run'}
            </button>
          </div>
        </div>

        {/* Tabs */}
        <div className="tabs" role="tablist">
          {[
            { id: 'logs',   label: 'Logs',          icon: <Terminal size={13} strokeWidth={1.5} /> },
            { id: 'docs',   label: 'Readme',         icon: <FileText size={13} strokeWidth={1.5} /> },
            { id: 'config', label: 'Configuration',  icon: <Settings2 size={13} strokeWidth={1.5} /> },
          ].map(t => (
            <button
              key={t.id}
              className={`tab ${tab === t.id ? 'active' : ''}`}
              onClick={() => setTab(t.id)}
              role="tab"
              aria-selected={tab === t.id}
              aria-label={`${t.label} tab`}
            >
              {t.icon} {t.label}
            </button>
          ))}
        </div>

        {/* Tab panels */}
        {tab === 'logs' && (
          <div className="terminal" role="tabpanel" aria-label="Log output">
            <div className="terminal-bar">
              <span className="terminal-bar-title">stdout · {capsule.scopedId}</span>
              <button
                className="btn btn-ghost"
                style={{padding:'3px 8px', fontSize:11}}
                onClick={onClearLogs}
                aria-label="Clear logs"
              >
                <RotateCcw size={11} strokeWidth={1.5} /> Clear
              </button>
            </div>
            <div ref={scrollRef} className="terminal-body">
              {(logs || []).length === 0 ? (
                <div className="term-empty" aria-live="polite">
                  — no output yet —
                </div>
              ) : (
                (logs || []).map((line, i) => (
                  <div key={i} className="log-line">
                    <span className="log-num">{i + 1}</span>
                    <span className={`log-text ${logClass(line)}`}>{line}</span>
                  </div>
                ))
              )}
            </div>
          </div>
        )}

        {tab === 'docs' && (
          <div className="docs-pane" role="tabpanel" aria-label="Readme">
            <ReadmeRenderer text={capsule.readme} />
          </div>
        )}

        {tab === 'config' && (
          <div className="config-pane" role="tabpanel" aria-label="Configuration">
            {/* TOML viewer */}
            <div className="config-section">
              <div className="config-section-header">
                <Braces size={13} strokeWidth={1.5} />
                capsule.toml
              </div>
              <TomlRenderer text={capsule.rawToml} />
            </div>

            {/* Env overrides */}
            <div className="config-section">
              <div className="config-section-header">
                <Settings2 size={13} strokeWidth={1.5} />
                Environment Variables
              </div>
              <div className="env-body">
                <p style={{fontSize:12, color:'var(--text-3)', marginBottom:4}}>
                  Overrides applied to the next spawned instance.
                </p>
                {Object.keys(capsule.envHints).map(key => (
                  <div key={key} className="env-row">
                    <span className="env-key">{key}</span>
                    <input
                      className="env-input"
                      type="text"
                      value={overrides[key] !== undefined ? overrides[key] : capsule.envHints[key]}
                      onChange={e => onEnvUpdate(capsule.id, key, e.target.value)}
                      aria-label={`Value for ${key}`}
                    />
                  </div>
                ))}
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
};

/* ─────────────────────────────────────────────
   PROCESS DRAWER
───────────────────────────────────────────── */
const ProcessDrawer = ({ isOpen, onClose, processes, onStop }) => {
  if (!isOpen) return null;
  return (
    <>
      <div className="drawer-overlay" onClick={onClose} role="presentation" />
      <div className="drawer" role="dialog" aria-modal="true" aria-label="Process manager">
        <div className="drawer-header">
          <div className="drawer-title">
            <Activity size={14} strokeWidth={1.5} color="var(--accent)" />
            Processes
          </div>
          <button className="icon-btn" onClick={onClose} aria-label="Close process manager">
            <X size={14} strokeWidth={1.5} />
          </button>
        </div>
        <div className="drawer-body">
          {processes.length === 0 ? (
            <div className="empty-state" style={{padding:'48px 16px'}} role="status">
              <Database size={28} strokeWidth={1} color="var(--border-md)" />
              <p>No running processes</p>
            </div>
          ) : processes.map(proc => (
            <div key={proc.pid} className="proc-card">
              <div className="proc-header">
                <div>
                  <div className="proc-name">{proc.scopedId}</div>
                  <div className="proc-pid">PID {proc.pid}</div>
                </div>
                <button
                  className="btn btn-danger"
                  style={{padding:'4px 8px'}}
                  onClick={() => onStop(proc.pid)}
                  aria-label={`Stop process ${proc.pid}`}
                >
                  <Trash2 size={12} strokeWidth={1.5} />
                </button>
              </div>
              <div className="proc-footer">
                <span style={{display:'flex', alignItems:'center', gap:4}}>
                  <Clock size={11} strokeWidth={1.5} />
                  {new Date(proc.startedAt).toLocaleTimeString()}
                </span>
                <span className="proc-status">
                  <span style={{width:5,height:5,borderRadius:'50%',background:'currentColor',animation:'pulse 2s infinite'}} />
                  Live
                </span>
              </div>
            </div>
          ))}
        </div>
      </div>
    </>
  );
};

/* ─────────────────────────────────────────────
   APP
───────────────────────────────────────────── */
export default function App() {
  const [view, setView]                     = useState('catalog');
  const [selectedId, setSelectedId]         = useState(null);
  const [searchQuery, setSearchQuery]       = useState('');
  const [targetFilter, setTargetFilter]     = useState('all');
  const [viewMode, setViewMode]             = useState('list');
  const [activeProcesses, setActiveProcesses] = useState({});
  const [logs, setLogs]                     = useState({});
  const [envOverrides, setEnvOverrides]     = useState({});
  const [drawerOpen, setDrawerOpen]         = useState(false);
  const [toast, setToast]                   = useState(null);

  const showToast = msg => {
    setToast(msg);
    setTimeout(() => setToast(null), 2500);
  };

  const selectedCapsule = useMemo(
    () => CAPSULES.find(c => c.id === selectedId),
    [selectedId]
  );

  const filteredCapsules = useMemo(() => {
    return CAPSULES.filter(c => {
      const text = `${c.scopedId} ${c.description} ${c.publisher}`.toLowerCase();
      if (!text.includes(searchQuery.toLowerCase())) return false;
      if (targetFilter === 'current')  return c.osArch.includes(CURRENT_TARGET);
      if (targetFilter === 'cross')    return c.osArch.length > 1;
      return true;
    });
  }, [searchQuery, targetFilter]);

  const addLog = (capsuleId, line) => {
    const t = new Date().toLocaleTimeString([], { hour12: false });
    setLogs(prev => ({
      ...prev,
      [capsuleId]: [...(prev[capsuleId] || []), `[${t}] ${line}`].slice(-200),
    }));
  };

  const startProcess = cap => {
    const pid = Math.floor(Math.random() * 90000) + 10000;
    const env = { ...cap.envHints, ...(envOverrides[cap.id] || {}) };
    const port = env.PORT || env.UVICORN_PORT || '8080';

    setActiveProcesses(prev => ({
      ...prev,
      [pid]: { pid, capsuleId: cap.id, scopedId: cap.scopedId, iconKey: cap.iconKey, startedAt: new Date(), env },
    }));

    addLog(cap.id, `INFO  Spawning container PID=${pid}`);
    addLog(cap.id, `INFO  Binding to 0.0.0.0:${port}`);
    addLog(cap.id, `INFO  Ready`);
    showToast(`Started ${cap.name} · PID ${pid}`);
  };

  const stopProcess = pid => {
    const proc = activeProcesses[pid];
    if (!proc) return;
    setActiveProcesses(prev => { const n = { ...prev }; delete n[pid]; return n; });
    addLog(proc.capsuleId, `WARN  SIGTERM → PID ${pid} stopped`);
    showToast(`Stopped PID ${pid}`);
  };

  const runningSessions = useMemo(() => {
    const seen = {};
    Object.values(activeProcesses).forEach(p => { seen[p.capsuleId] = p; });
    return Object.values(seen);
  }, [activeProcesses]);

  return (
    <>
      <FontStyle />
      <div className="app">
        <Sidebar
          view={view}
          setView={v => { setView(v); }}
          activeSessions={runningSessions}
          onSessionClick={id => { setSelectedId(id); setView('detail'); }}
          onOpenDrawer={() => setDrawerOpen(true)}
        />

        {view === 'catalog' ? (
          <CatalogPage
            capsules={filteredCapsules}
            searchQuery={searchQuery}
            setSearchQuery={setSearchQuery}
            targetFilter={targetFilter}
            setTargetFilter={setTargetFilter}
            viewMode={viewMode}
            setViewMode={setViewMode}
            activeProcesses={activeProcesses}
            onStart={startProcess}
            onInspect={id => { setSelectedId(id); setView('detail'); }}
          />
        ) : selectedCapsule ? (
          <DetailPage
            capsule={selectedCapsule}
            onBack={() => setView('catalog')}
            onStart={startProcess}
            activeProcs={Object.values(activeProcesses).filter(p => p.capsuleId === selectedCapsule.id)}
            onStop={stopProcess}
            logs={logs[selectedCapsule.id]}
            onClearLogs={() => setLogs(prev => ({ ...prev, [selectedCapsule.id]: [] }))}
            overrides={envOverrides[selectedCapsule.id] || {}}
            onEnvUpdate={(cid, k, v) =>
              setEnvOverrides(prev => ({ ...prev, [cid]: { ...(prev[cid] || {}), [k]: v } }))
            }
          />
        ) : null}

        <ProcessDrawer
          isOpen={drawerOpen}
          onClose={() => setDrawerOpen(false)}
          processes={Object.values(activeProcesses)}
          onStop={stopProcess}
        />

        {toast && (
          <div className="toast" role="status" aria-live="polite">
            <CheckCircle2 size={13} strokeWidth={2} color="#4ade80" />
            {toast}
          </div>
        )}
      </div>
    </>
  );
}
