import React, { useEffect, useMemo, useState } from 'react'
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
const progressLabels = {
  queued: 'Queued',
  downloading: 'Downloading',
  verifying: 'Verifying',
  installing: 'Installing',
  ready: 'Ready',
  failed: 'Failed',
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
        const queued = Object.fromEntries((payload.runtimeInstallStarted.tools || []).map((tool) => [
          tool,
          { phase: 'queued', message: 'Queued' },
        ]))
        setProgressByTool(queued)
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
        setInstalling(false)
        if (complete.status) {
          setRuntimeStatus(complete.status)
        }
        setInstallError(complete.success ? null : complete.error || 'Runtime install failed')
      }
      if (payload.runtimeInstallCancelled) {
        setInstallError('Cancelling runtime install...')
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
    BRIDGE({ kind: 'load_runtime_setup_status', request_id: nextRequestId() })
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
  const hasInstallTargets = selectedInstallTools.length > 0
  const failedInstall = Object.values(progressByTool).some((progress) => progress.phase === 'failed')
  const primaryLabel = installing
    ? 'Installing selected tools...'
    : checking
      ? 'Checking tools...'
      : hasInstallTargets
        ? failedInstall
          ? 'Retry selected tools'
          : 'Install selected tools'
        : 'Continue'
  const primaryDisabled = installing || checking

  const startInstall = () => {
    if (!hasInstallTargets) return
    setInstalling(true)
    setInstallError(null)
    setProgressByTool(Object.fromEntries(selectedInstallTools.map((tool) => [
      tool,
      { phase: 'queued', message: 'Queued' },
    ])))
    BRIDGE({
      kind: 'install_runtime_tools',
      request_id: nextRequestId(),
      tools: selectedInstallTools,
    })
  }

  const cancelInstall = () => {
    setInstallError('Cancelling runtime install...')
    BRIDGE({ kind: 'cancel_runtime_install', request_id: nextRequestId() })
  }

  const handlePrimary = () => {
    if (hasInstallTargets) startInstall()
    else onFinish()
  }

  const podmanPill = detectionStatusPill({ checked: podmanEnabled, tool: tools.podman })
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

      <div className="flex-1 min-h-0 overflow-y-auto flex flex-col gap-3">
        <p className="text-[12px] font-bold tracking-widest text-slate-400 uppercase">
          Container runtime
        </p>
        <ToggleCard
          checked={podmanEnabled}
          onToggle={() => setPodmanEnabled((v) => !v)}
          disabled={installing}
          icon={Container}
          title="Use existing Podman for containers"
          status={podmanPill.label}
          statusTone={podmanPill.tone}
          control="switch"
          footer={
            <div className="flex flex-col gap-2">
              <div className="flex flex-wrap gap-2">
                <StatusPill tone="amber">Detection only</StatusPill>
                <StatusPill tone="slate">Ato will not install Podman automatically</StatusPill>
              </div>
              {tools.podman?.message && (
                <p className="text-[12px] leading-snug text-slate-500">{tools.podman.message}</p>
              )}
            </div>
          }
        >
          Lets Ato use Podman as a container engine when a recipe needs one. If
          Podman or Docker Desktop is missing, Ato shows setup instructions
          instead of installing a container runtime.
        </ToggleCard>

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

      <div className={`shrink-0 mt-6 grid gap-3 ${installing || hasInstallTargets ? 'grid-cols-[0.75fr_1.25fr]' : 'grid-cols-1'}`}>
        {(installing || hasInstallTargets) && (
          <button
            onClick={installing ? cancelInstall : onFinish}
            className="w-full py-4 bg-white border border-slate-200 text-slate-600 rounded-2xl font-bold text-[15px] hover:bg-slate-50 transition-colors flex justify-center items-center"
          >
            {installing ? 'Cancel install' : 'Skip for now'}
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
