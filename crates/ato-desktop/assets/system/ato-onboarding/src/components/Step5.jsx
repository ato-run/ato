import React, { useEffect, useMemo, useRef, useState } from 'react'
import { ShieldCheck, Box, Hexagon, FileCode, Container, Wrench, ShipWheel } from 'lucide-react'
import { BRIDGE } from '../bridge'

const pillTones = {
  violet: 'bg-violet-100 text-violet-700 border-violet-200',
  amber: 'bg-amber-100 text-amber-700 border-amber-200',
  emerald: 'bg-emerald-100 text-emerald-700 border-emerald-200',
  slate: 'bg-slate-100 text-slate-600 border-slate-200',
  rose: 'bg-rose-100 text-rose-700 border-rose-200',
}

const installActionKinds = new Set(['install_managed', 'upgrade_managed'])
// Podman status actions that an explicit user "Prepare" can resolve. Anything
// else (e.g. `open_instructions` for an unsupported host or a missing package
// manager) is not auto-preparable — the UI shows guidance and a skip path.
const podmanPrepareActions = new Set([
  'prepare_host_runtime',
  'start_service',
  'repair_host_runtime',
])
const progressLabels = {
  queued: 'Queued',
  downloading: 'Downloading',
  verifying: 'Verifying',
  installing: 'Installing',
  ready: 'Ready',
  failed: 'Failed',
}
// Host-runtime prepare phases (PR #440) → human display. Keyed off the
// structured `phase` token, never an English backend message.
const podmanPhaseLabels = {
  queued: 'Queued',
  locating: 'Checking Podman',
  installing: 'Installing Podman',
  initializing_machine: 'Creating ato-podman',
  starting_machine: 'Starting ato-podman',
  verifying: 'Verifying Podman',
  ready: 'Podman is ready',
  failed: 'Podman setup failed',
}

let requestCounter = 0
function nextRequestId() {
  requestCounter += 1
  return `runtime-setup-${requestCounter}`
}

function statusByKind(status) {
  return Object.fromEntries((status?.tools || []).map((tool) => [tool.kind, tool]))
}

function isInstallNeeded(tool) {
  return tool && !tool.ready && installActionKinds.has(tool.action)
}

function runtimeStatusPill({ checked, tool, progress, fallback = 'Will install when needed' }) {
  if (!checked) return { label: 'Disabled', tone: 'slate' }
  if (progress?.phase) {
    return {
      label: progressLabels[progress.phase] || progress.phase,
      tone: progress.phase === 'failed' ? 'rose' : progress.phase === 'ready' ? 'emerald' : 'amber',
    }
  }
  if (!tool) return { label: 'Checking', tone: 'slate' }
  if (tool.ready) return { label: 'Ready', tone: 'emerald' }
  if (isInstallNeeded(tool)) return { label: fallback, tone: 'violet' }
  if (tool.installed && !tool.supported) return { label: 'Unsupported', tone: 'rose' }
  return { label: 'Missing', tone: 'amber' }
}

function detectionStatusPill({ checked = true, tool }) {
  if (!checked) return { label: 'Disabled', tone: 'slate' }
  if (!tool) return { label: 'Checking', tone: 'slate' }
  if (tool.ready) return { label: 'Ready', tone: 'emerald' }
  if (tool.installed && tool.action === 'start_service') return { label: 'Not running', tone: 'amber' }
  if (tool.installed && !tool.supported) return { label: 'Unsupported', tone: 'rose' }
  return { label: 'Missing', tone: 'amber' }
}

// Podman is a host runtime Ato can *prepare* on explicit opt-in (install +
// create/start the `ato-podman` machine). Classify from the structured
// `ready`/`action` fields and any live prepare progress — never message text.
function podmanStatusPill({ checked, tool, progress }) {
  if (!checked) return { label: 'Disabled', tone: 'slate' }
  if (progress?.phase) {
    return {
      label: podmanPhaseLabels[progress.phase] || progress.phase,
      tone: progress.phase === 'failed' ? 'rose' : progress.phase === 'ready' ? 'emerald' : 'amber',
    }
  }
  if (!tool) return { label: 'Checking', tone: 'slate' }
  if (tool.ready) return { label: 'Ready', tone: 'emerald' }
  switch (tool.action) {
    case 'prepare_host_runtime':
      return { label: 'Needs setup', tone: 'violet' }
    case 'start_service':
      return { label: 'Stopped', tone: 'amber' }
    case 'repair_host_runtime':
      return { label: 'Needs repair', tone: 'amber' }
    case 'open_instructions':
      return { label: tool.installed ? 'Unsupported' : 'Not installed', tone: 'amber' }
    default:
      return { label: tool.installed ? 'Installed' : 'Missing', tone: 'amber' }
  }
}

function bundledStatusPill(tool) {
  if (!tool) return { label: 'Checking', tone: 'slate' }
  if (tool.ready && tool.source === 'bundled') return { label: 'Bundled', tone: 'emerald' }
  if (tool.ready) return { label: 'Ready', tone: 'emerald' }
  return { label: 'Missing', tone: 'rose' }
}

function StatusPill({ children, tone = 'violet', className = '' }) {
  return (
    <span
      className={`inline-flex shrink-0 items-center rounded-full border px-2.5 py-1 text-[11px] font-bold leading-none ${pillTones[tone]} ${className}`}
    >
      {children}
    </span>
  )
}

function CardControl({ checked, kind }) {
  if (kind === 'switch') {
    return (
      <span
        className={`mt-0.5 flex h-6 w-11 shrink-0 items-center rounded-full p-1 transition-colors ${
          checked ? 'bg-[#8B5CF6]' : 'bg-slate-300'
        }`}
        aria-hidden="true"
      >
        <span
          className={`h-4 w-4 rounded-full bg-white shadow-sm transition-transform ${
            checked ? 'translate-x-5' : 'translate-x-0'
          }`}
        />
      </span>
    )
  }

  return (
    <span
      className={`mt-0.5 w-6 h-6 shrink-0 rounded-md border flex items-center justify-center transition-colors ${
        checked ? 'bg-[#8B5CF6] border-[#8B5CF6]' : 'bg-white border-slate-300'
      }`}
      aria-hidden="true"
    >
      {checked && (
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none">
          <path d="M5 13l4 4L19 7" stroke="white" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      )}
    </span>
  )
}

// Runtime Setup (issue #420 revision): instead of scanning host devices, Ato
// checks the *runtime tools* recipes need and installs Ato-managed language
// runtimes when required. These toggles are default-on, opt-out preferences.
// State is owned by App so the keyboard "finish" path and the button submit the
// same values.
function ToggleCard({
  checked,
  onToggle,
  disabled = false,
  icon: Icon,
  title,
  status,
  statusTone = 'violet',
  control = 'checkbox',
  footer,
  children,
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      disabled={disabled}
      className={`w-full text-left rounded-[20px] border p-4 flex gap-3 items-start transition-all ${
        checked ? 'bg-[#F5F3FF] border-[#DDD6FE]' : 'bg-white border-slate-200'
      } ${disabled ? 'cursor-not-allowed opacity-75' : ''}`}
    >
      <CardControl checked={checked} kind={control} />
      <span className="min-w-0 flex-1">
        <span className="mb-1 flex items-start gap-2">
          <span className="flex min-w-0 items-center gap-2">
            <Icon className="shrink-0 text-[#8B5CF6]" size={18} strokeWidth={2} />
            <span className="font-bold text-[#0F172A] text-[15px] leading-tight">{title}</span>
          </span>
          <StatusPill tone={statusTone} className="ml-auto">
            {status}
          </StatusPill>
        </span>
        <span className="block text-[13px] text-slate-500 leading-snug">{children}</span>
        {footer && <div className="mt-3">{footer}</div>}
      </span>
    </button>
  )
}

function SystemCheck({ icon: Icon, label, status, tone, detail }) {
  return (
    <div className="rounded-2xl border border-slate-200 bg-white p-3">
      <div className="mb-2 flex items-center gap-2">
        <Icon className="text-slate-400" size={16} strokeWidth={2} />
        <span className="text-[12px] font-bold text-[#0F172A]">{label}</span>
      </div>
      <StatusPill tone={tone}>{status}</StatusPill>
      <p className="mt-2 text-[11px] leading-snug text-slate-500">{detail}</p>
    </div>
  )
}

// #460 PR2: Windows substrate (WSL / virtualization / reboot / Podman machine
// health) card with Desktop-driven CTAs. Renders only on Windows (where the
// `windows_substrate` field is present). Never tells the user to open a shell.
function WindowsSubstrateCard({ substrate, podmanTool, installing, onAction, onRepair, onResume }) {
  if (!substrate) return null
  const wsl = substrate.wsl
  const action = substrate.action || { kind: 'none' }
  const healthError = !!podmanTool && podmanTool.action === 'repair_host_runtime'

  if (wsl === 'ready' && !healthError) {
    return (
      <div className="rounded-2xl border border-emerald-200 bg-emerald-50 px-4 py-3">
        <p className="text-[13px] font-bold text-emerald-800">Windows substrate ready</p>
        <p className="text-[12px] leading-snug text-emerald-700">
          {substrate.message || 'WSL2 is available.'}
        </p>
      </div>
    )
  }

  const btn =
    'self-start rounded-lg bg-[#8B5CF6] px-3 py-1.5 text-[12px] font-semibold text-white disabled:opacity-50'
  const linkBtn =
    'self-start text-[12px] font-semibold text-[#8B5CF6] underline disabled:opacity-50'
  let cta = null
  if (action.kind === 'reboot_required') {
    // `reboot_required` persists the resume marker via prepare-windows-substrate
    // (NOT resume, which is read-only) so setup can continue after the restart.
    // The secondary link is the explicit post-restart continuation.
    cta = (
      <div className="flex flex-col gap-1.5">
        <button
          type="button"
          disabled={installing}
          onClick={() => onAction('reboot_required')}
          className={btn}
        >
          Save and restart
        </button>
        <button type="button" disabled={installing} onClick={onResume} className={linkBtn}>
          Already restarted? Continue
        </button>
      </div>
    )
  } else if (healthError || action.kind === 'repair_podman_machine') {
    cta = (
      <button type="button" disabled={installing} onClick={onRepair} className={btn}>
        Repair Ato Podman machine
      </button>
    )
  } else if (action.kind && action.kind !== 'none' && action.can_run_from_desktop) {
    cta = (
      <button
        type="button"
        disabled={installing}
        onClick={() => onAction(action.kind)}
        className={btn}
      >
        {action.label || 'Fix'}
      </button>
    )
  }

  // For a Podman machine health error the substrate message ("WSL2 is
  // available") is misleading — describe the machine fault instead.
  const detail = healthError
    ? podmanTool?.message || 'The Ato Podman machine is running but not responding.'
    : action.description || substrate.message

  return (
    <div className="flex flex-col gap-2 rounded-2xl border border-amber-200 bg-amber-50 px-4 py-3">
      <p className="text-[13px] font-bold text-amber-800">Windows container substrate</p>
      <p className="text-[12px] leading-snug text-amber-700">{detail}</p>
      {action.requires_admin ? (
        <p className="text-[11px] leading-snug text-amber-600">
          Needs administrator approval; Ato will request it.
        </p>
      ) : null}
      {cta}
      {action.kind === 'open_virtualization_instructions' ? (
        <p className="text-[11px] leading-snug text-amber-600">
          Some steps may require firmware/BIOS changes Ato can’t make for you.
        </p>
      ) : null}
    </div>
  )
}

export default function Step5({
  onFinish,
  podmanEnabled,
  setPodmanEnabled,
  nodeInstallEnabled,
  setNodeInstallEnabled,
  uvInstallEnabled,
  setUvInstallEnabled,
  pythonInstallEnabled,
  setPythonInstallEnabled,
}) {
  const [checking, setChecking] = useState(true)
  const [runtimeStatus, setRuntimeStatus] = useState(null)
  const [runtimeError, setRuntimeError] = useState(null)
  const [installing, setInstalling] = useState(false)
  const [installError, setInstallError] = useState(null)
  const [progressByTool, setProgressByTool] = useState({})
  // Which streamed job is running ('install' | 'prepare' | null) — drives the
  // busy label and lets the completion handler chain managed-install → Podman
  // prepare. Mirrored into refs so the once-bound hydrate listener and key
  // handler read the latest value without re-subscribing.
  const [activeJob, setActiveJob] = useState(null)
  const activeJobRef = useRef(null)
  activeJobRef.current = activeJob
  const prepareQueuedRef = useRef(false)
  const startPrepareRef = useRef(() => {})
  // #460 PR3b: a capsule launch the user attempted before Runtime Setup was
  // ready. Once setup completes, the Desktop resumes it automatically; until
  // then we show a banner so the user knows why they're here.
  const [pendingLaunch, setPendingLaunch] = useState(null)
  const [resumeError, setResumeError] = useState(null)

  useEffect(() => {
    const previousHydrate = window.__ATO_ONBOARDING_HYDRATE__
    window.__ATO_ONBOARDING_HYDRATE__ = (payload) => {
      window.dispatchEvent(new CustomEvent('ato-onboarding-runtime-setup', { detail: payload }))
    }

    const onHydrate = (event) => {
      const payload = event.detail || {}
      if (payload.runtimeSetupStatus) {
        setRuntimeStatus(payload.runtimeSetupStatus)
        setRuntimeError(null)
        setChecking(false)
      }
      if (payload.runtimeInstallStarted) {
        // Merge (don't replace): a chained Podman prepare must not erase the
        // managed-tool rows that already finished, so the combined activity
        // list stays intact.
        const queued = Object.fromEntries((payload.runtimeInstallStarted.tools || []).map((tool) => [
          tool,
          { phase: 'queued', message: 'Queued' },
        ]))
        setProgressByTool((current) => ({ ...current, ...queued }))
        setInstalling(true)
        setInstallError(null)
      }
      if (payload.runtimeInstallProgress) {
        const event = payload.runtimeInstallProgress
        if (event.tool) {
          setProgressByTool((current) => ({
            ...current,
            [event.tool]: { phase: event.phase, message: event.message },
          }))
        }
      }
      if (payload.runtimeInstallComplete) {
        const complete = payload.runtimeInstallComplete
        if (complete.status) {
          setRuntimeStatus(complete.status)
        }
        if (complete.success && activeJobRef.current === 'install' && prepareQueuedRef.current) {
          // Managed install finished — chain straight into Podman prepare
          // without dropping the busy state (one user click, sequential work).
          prepareQueuedRef.current = false
          setInstallError(null)
          startPrepareRef.current()
        } else {
          setInstalling(false)
          setActiveJob(null)
          activeJobRef.current = null
          prepareQueuedRef.current = false
          setInstallError(complete.success ? null : complete.error || 'Runtime setup failed')
        }
      }
      if (payload.runtimeInstallCancelled) {
        setInstallError('Cancelling runtime install...')
      }
      // #460 PR3b: pending interrupted-launch banner state. `pendingLaunch` is
      // explicitly `null` when there is nothing to resume.
      if ('pendingLaunch' in payload) {
        setPendingLaunch(payload.pendingLaunch)
      }
      if (payload.pendingLaunchCancelled) {
        setPendingLaunch(null)
      }
      if (payload.launchResumeFailed) {
        setResumeError((payload.error && payload.error.message) || 'Could not resume the pending launch.')
      }
      if (payload.runtimeSetupResume) {
        // #460 PR2: resume-after-reboot carries a refreshed status snapshot.
        const resumed = payload.runtimeSetupResume.runtimeSetupStatus
        if (resumed) {
          setRuntimeStatus(resumed)
          setChecking(false)
        }
      }
      if (payload.error && !payload.runtimeInstallComplete) {
        setChecking(false)
        setInstalling(false)
        const message = payload.error.message || 'Runtime setup failed'
        if (payload.runtimeInstallStarted || payload.runtimeInstallProgress) {
          setInstallError(message)
        } else {
          setRuntimeError(message)
        }
      }
    }

    window.addEventListener('ato-onboarding-runtime-setup', onHydrate)
    BRIDGE({ kind: 'runtime_setup_status', request_id: nextRequestId() })
    return () => {
      window.removeEventListener('ato-onboarding-runtime-setup', onHydrate)
      window.__ATO_ONBOARDING_HYDRATE__ = previousHydrate
    }
  }, [])

  const tools = useMemo(() => statusByKind(runtimeStatus), [runtimeStatus])
  const languageCards = [
    {
      kind: 'node',
      checked: nodeInstallEnabled,
      setChecked: setNodeInstallEnabled,
      icon: Hexagon,
      title: 'Install Ato-managed Node.js when needed',
      body: "Uses Ato's toolchain cache instead of relying on a system Node.js, so JavaScript recipes run the same on every machine.",
    },
    {
      kind: 'uv',
      checked: uvInstallEnabled,
      setChecked: setUvInstallEnabled,
      icon: Box,
      title: 'Install Ato-managed uv when needed',
      body: 'Uses an Ato-supported uv from the toolchain cache for Python recipes that build with uv.',
    },
    {
      kind: 'python',
      checked: pythonInstallEnabled,
      setChecked: setPythonInstallEnabled,
      icon: FileCode,
      title: 'Install Ato-managed Python when needed',
      body: 'Uses Ato-supported Python 3.12 from the toolchain cache for recipes that need Python.',
    },
  ]
  const selectedInstallTools = languageCards
    .filter((card) => card.checked && isInstallNeeded(tools[card.kind]))
    .map((card) => card.kind)
  const managedPending = selectedInstallTools.length > 0

  // Podman is pending only when the user opted in, it isn't already ready, and
  // its recommended action is one an explicit Prepare can resolve. This is the
  // single source of truth for "the primary action must prepare, not finish".
  const podmanTool = tools.podman
  const podmanShouldPrepare =
    podmanEnabled &&
    !!podmanTool &&
    !podmanTool.ready &&
    podmanPrepareActions.has(podmanTool.action)
  // Opted-in but not preparable (e.g. unsupported host / no package manager):
  // show guidance, never trap — Continue still finishes, skip still offered.
  const podmanNeedsInstructions =
    podmanEnabled && !!podmanTool && !podmanTool.ready && podmanTool.action === 'open_instructions'

  const hasPendingWork = managedPending || podmanShouldPrepare
  const failedProgress = Object.values(progressByTool).some((progress) => progress.phase === 'failed')

  let primaryLabel
  if (installing) {
    primaryLabel = activeJob === 'prepare' ? 'Preparing Podman...' : 'Installing selected tools...'
  } else if (checking) {
    primaryLabel = 'Checking tools...'
  } else if (managedPending && podmanShouldPrepare) {
    primaryLabel = failedProgress ? 'Retry setup' : 'Install and prepare selected tools'
  } else if (managedPending) {
    primaryLabel = failedProgress ? 'Retry selected tools' : 'Install selected tools'
  } else if (podmanShouldPrepare) {
    primaryLabel = failedProgress ? 'Retry Podman setup' : 'Prepare Podman'
  } else {
    primaryLabel = 'Continue'
  }
  const primaryDisabled = installing || checking

  const saveSettings = () => {
    BRIDGE({
      kind: 'save_runtime_setup_settings',
      podman_enabled: podmanEnabled,
      node_install_enabled: nodeInstallEnabled,
      uv_install_enabled: uvInstallEnabled,
      python_install_enabled: pythonInstallEnabled,
    })
  }

  const startInstall = (prepareAfter) => {
    setInstalling(true)
    setInstallError(null)
    setActiveJob('install')
    activeJobRef.current = 'install'
    prepareQueuedRef.current = !!prepareAfter
    setProgressByTool((current) => ({
      ...current,
      ...Object.fromEntries(selectedInstallTools.map((tool) => [tool, { phase: 'queued', message: 'Queued' }])),
    }))
    BRIDGE({
      kind: 'install_runtime_tools',
      request_id: nextRequestId(),
      tools: selectedInstallTools,
    })
  }

  const startPrepare = () => {
    setInstalling(true)
    setInstallError(null)
    setActiveJob('prepare')
    activeJobRef.current = 'prepare'
    prepareQueuedRef.current = false
    setProgressByTool((current) => ({ ...current, podman: { phase: 'queued', message: 'Queued' } }))
    BRIDGE({
      kind: 'prepare_runtime_tools',
      request_id: nextRequestId(),
      tools: ['podman'],
    })
  }
  // Keep the ref pointed at the latest closure so the completion handler can
  // chain managed-install → Podman prepare.
  startPrepareRef.current = startPrepare

  const cancelInstall = () => {
    setInstallError('Cancelling runtime setup...')
    BRIDGE({ kind: 'cancel_runtime_install', request_id: nextRequestId() })
  }

  // #460 PR2: Windows substrate remediations. These stream through the same
  // runtimeInstall* progress/complete events as prepare, so they reuse the busy
  // and error state — no shell required of the user.
  const runSubstrateAction = (actionKind) => {
    setInstalling(true)
    setInstallError(null)
    setActiveJob('substrate')
    activeJobRef.current = 'substrate'
    prepareQueuedRef.current = false
    setProgressByTool((current) => ({ ...current, substrate: { phase: 'queued', message: 'Queued' } }))
    BRIDGE({
      kind: 'prepare_windows_runtime_substrate',
      request_id: nextRequestId(),
      action: actionKind,
      source_surface: 'onboarding',
    })
  }

  const runRepair = () => {
    setInstalling(true)
    setInstallError(null)
    setActiveJob('repair')
    activeJobRef.current = 'repair'
    prepareQueuedRef.current = false
    setProgressByTool((current) => ({ ...current, podman: { phase: 'queued', message: 'Queued' } }))
    BRIDGE({ kind: 'repair_host_runtime', request_id: nextRequestId() })
  }

  const runResume = () => {
    BRIDGE({ kind: 'resume_runtime_setup_after_reboot', request_id: nextRequestId() })
  }

  // #460 PR3b: dismiss a pending interrupted-launch. Clears the launch-intent
  // marker only — Runtime Setup itself keeps running.
  const cancelPendingLaunch = () => {
    setPendingLaunch(null)
    setResumeError(null)
    BRIDGE({ kind: 'cancel_pending_launch', request_id: nextRequestId() })
  }

  const skipPodman = () => {
    // Explicit opt-out: disable Podman locally and persist false deterministically
    // (via the finish override) so a failed/unavailable prepare never traps the
    // user on the final step.
    setPodmanEnabled(false)
    onFinish({ podman_enabled: false })
  }

  const handlePrimary = () => {
    if (installing || checking) return
    if (!hasPendingWork) {
      onFinish()
      return
    }
    // 1) persist settings  2) managed install (if selected)  3) Podman prepare.
    // Install and prepare never run in parallel — prepare is chained after the
    // install completes in the runtimeInstallComplete handler.
    saveSettings()
    if (managedPending) {
      startInstall(podmanShouldPrepare)
    } else {
      startPrepare()
    }
  }

  // App.jsx defers Enter/ArrowRight on the final step to us, so the keyboard
  // path runs the *same* action as the primary button (install-then-finish or
  // finish) instead of skipping a pending install. A ref keeps the listener
  // bound once while always invoking the latest closure/state.
  const primaryRef = useRef(() => {})
  primaryRef.current = () => {
    if (!primaryDisabled) handlePrimary()
  }
  useEffect(() => {
    const onKey = (e) => {
      if (e.key === 'Enter' || e.key === 'ArrowRight') {
        e.preventDefault()
        primaryRef.current()
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  const podmanPill = podmanStatusPill({
    checked: podmanEnabled,
    tool: tools.podman,
    progress: progressByTool.podman,
  })
  const helperPill = bundledStatusPill(tools.ato_helper)
  const nacellePill = bundledStatusPill(tools.nacelle)
  const dockerPill = detectionStatusPill({ tool: tools.docker_desktop })

  return (
    <div className="flex flex-col h-full p-8">
      <div className="shrink-0">
        <div className="text-[#8B5CF6] font-bold tracking-widest text-sm mb-4 mt-2">5 / 5</div>

        <div className="flex items-center gap-2 mb-2">
          <ShieldCheck className="text-[#8B5CF6]" size={28} strokeWidth={2} />
          <h1 className="text-[36px] leading-tight font-extrabold text-[#0F172A] tracking-tight">
            Runtime Setup
          </h1>
        </div>

        <p className="text-[15px] text-slate-500 mb-5 pr-4 leading-relaxed">
          Ato checks the tools needed to run recipes on this machine. For
          language runtimes, Ato can install managed versions so recipes run
          consistently. These are on by default — change them later in Settings.
        </p>
      </div>

      {pendingLaunch && (
        <div className="shrink-0 mb-3 rounded-xl border border-violet-200 bg-violet-50 px-4 py-3">
          <div className="flex items-start justify-between gap-3">
            <p className="text-[13px] leading-snug text-violet-800">
              After setup, Ato will continue opening{' '}
              <span className="font-semibold">{pendingLaunch.label || 'your app'}</span>.
            </p>
            <button
              type="button"
              onClick={cancelPendingLaunch}
              className="shrink-0 text-[12px] font-medium text-violet-600 hover:text-violet-800 underline"
            >
              Cancel
            </button>
          </div>
        </div>
      )}
      {resumeError && (
        <div className="shrink-0 mb-3 rounded-xl border border-rose-200 bg-rose-50 px-4 py-3">
          <p className="text-[13px] leading-snug text-rose-700">{resumeError}</p>
        </div>
      )}

      <div className="flex-1 min-h-0 overflow-y-auto flex flex-col gap-3">
        <p className="text-[12px] font-bold tracking-widest text-slate-400 uppercase">
          Container apps
        </p>
        <ToggleCard
          checked={podmanEnabled}
          onToggle={() => setPodmanEnabled((v) => !v)}
          disabled={installing}
          icon={Container}
          title="Use Podman for container apps"
          status={podmanPill.label}
          statusTone={podmanPill.tone}
          control="switch"
          footer={
            <div className="flex flex-col gap-2">
              {progressByTool.podman?.message ? (
                <p className="text-[12px] leading-snug text-slate-500">{progressByTool.podman.message}</p>
              ) : tools.podman?.message ? (
                <p className="text-[12px] leading-snug text-slate-500">{tools.podman.message}</p>
              ) : null}
              {!installing && podmanShouldPrepare && (
                <p className="text-[12px] leading-snug text-violet-600">
                  Ato will set this up when you continue — this may download packages or a VM image.
                </p>
              )}
              {!installing && podmanNeedsInstructions && (
                <p className="text-[12px] leading-snug text-amber-700">
                  Ato can’t install Podman automatically here. Follow the guidance above, or skip Podman for now.
                </p>
              )}
            </div>
          }
        >
          Ato uses Podman to run container-based apps locally. When supported,
          Ato can install Podman and create/start an Ato-managed machine named
          ato-podman after you confirm. This may download packages or a VM image.
        </ToggleCard>

        {podmanEnabled && runtimeStatus?.windows_substrate ? (
          <WindowsSubstrateCard
            substrate={runtimeStatus.windows_substrate}
            podmanTool={tools.podman}
            installing={installing}
            onAction={runSubstrateAction}
            onRepair={runRepair}
            onResume={runResume}
          />
        ) : null}

        <p className="mt-2 text-[12px] font-bold tracking-widest text-slate-400 uppercase">
          Ato-managed language runtimes
        </p>
        {languageCards.map((card) => {
          const pill = runtimeStatusPill({
            checked: card.checked,
            tool: tools[card.kind],
            progress: progressByTool[card.kind],
          })
          return (
            <ToggleCard
              key={card.kind}
              checked={card.checked}
              onToggle={() => card.setChecked((v) => !v)}
              disabled={installing}
              icon={card.icon}
              title={card.title}
              status={pill.label}
              statusTone={pill.tone}
              footer={
                <div className="flex flex-col gap-2">
                  {progressByTool[card.kind]?.message && (
                    <p className="text-[12px] leading-snug text-slate-500">{progressByTool[card.kind].message}</p>
                  )}
                  {!progressByTool[card.kind]?.message && tools[card.kind]?.message && (
                    <p className="text-[12px] leading-snug text-slate-500">{tools[card.kind].message}</p>
                  )}
                </div>
              }
            >
              {card.body}
            </ToggleCard>
          )
        })}

        <div className="mt-2">
          <p className="mb-2 text-[12px] font-bold tracking-widest text-slate-400 uppercase">
            System checks
          </p>
          <div className="grid grid-cols-3 gap-3">
            <SystemCheck
              icon={Wrench}
              label="Ato helper"
              status={helperPill.label}
              tone={helperPill.tone}
              detail={tools.ato_helper?.message || 'Ships with Desktop.'}
            />
            <SystemCheck
              icon={ShipWheel}
              label="Nacelle"
              status={nacellePill.label}
              tone={nacellePill.tone}
              detail={tools.nacelle?.message || 'Source runtime included.'}
            />
            <SystemCheck
              icon={Container}
              label="Docker Desktop"
              status={dockerPill.label}
              tone={dockerPill.tone}
              detail={tools.docker_desktop?.message || 'Ato checks it, but never installs it.'}
            />
          </div>
        </div>

        {(runtimeError || installError) && (
          <div className="rounded-2xl border border-amber-200 bg-amber-50 p-3 text-[12px] leading-snug text-amber-800">
            {installError || runtimeError}
          </div>
        )}
      </div>

      {/* #460 PR3 (Case A): once setup is ready with nothing pending, offer to
          resume straight into a lightweight sample app instead of just finishing.
          pgweb is single-service, secret-free, and a good Podman smoke. */}
      {!installing && !checking && !hasPendingWork && podmanEnabled && !!tools.podman?.ready && (
        <button
          type="button"
          onClick={() => onFinish({}, 'capsule://github.com/sosedoff/pgweb')}
          className="shrink-0 mt-6 w-full py-3 bg-white border border-violet-200 text-[#8B5CF6] rounded-2xl font-bold text-[15px] hover:bg-violet-50 transition-colors flex justify-center items-center gap-2"
        >
          Continue to a sample app <span className="text-lg">→</span>
        </button>
      )}

      <div className={`shrink-0 mt-6 grid gap-3 ${installing || hasPendingWork ? 'grid-cols-[0.75fr_1.25fr]' : 'grid-cols-1'}`}>
        {(installing || hasPendingWork) && (
          <button
            onClick={installing ? cancelInstall : podmanShouldPrepare ? skipPodman : onFinish}
            className="w-full py-4 bg-white border border-slate-200 text-slate-600 rounded-2xl font-bold text-[15px] hover:bg-slate-50 transition-colors flex justify-center items-center"
          >
            {installing ? 'Cancel' : podmanShouldPrepare ? 'Skip Podman for now' : 'Skip for now'}
          </button>
        )}
        <button
          onClick={handlePrimary}
          disabled={primaryDisabled}
          className={`w-full py-4 bg-gradient-to-r from-[#A78BFA] to-[#8B5CF6] text-white rounded-2xl font-bold text-[17px] shadow-lg shadow-violet-500/25 transition-opacity flex justify-center items-center gap-2 ${
            primaryDisabled ? 'opacity-60 cursor-not-allowed' : 'hover:opacity-90'
          }`}
        >
          {primaryLabel} <span className="text-xl">→</span>
        </button>
      </div>
    </div>
  )
}
