import React from 'react'
import { ShieldCheck, Box, Hexagon, FileCode, Container, Wrench, ShipWheel } from 'lucide-react'

const pillTones = {
  violet: 'bg-violet-100 text-violet-700 border-violet-200',
  amber: 'bg-amber-100 text-amber-700 border-amber-200',
  emerald: 'bg-emerald-100 text-emerald-700 border-emerald-200',
  slate: 'bg-slate-100 text-slate-600 border-slate-200',
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
      className={`w-full text-left rounded-[20px] border p-4 flex gap-3 items-start transition-all ${
        checked ? 'bg-[#F5F3FF] border-[#DDD6FE]' : 'bg-white border-slate-200'
      }`}
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
        {footer && <span className="mt-3 block">{footer}</span>}
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
  const selectedManagedTools = [nodeInstallEnabled, uvInstallEnabled, pythonInstallEnabled].filter(Boolean).length
  const ctaLabel = selectedManagedTools > 0 ? 'Install selected tools' : 'Continue'

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
          icon={Container}
          title="Use existing Podman when available"
          status={podmanEnabled ? 'Detection only' : 'Disabled'}
          statusTone={podmanEnabled ? 'amber' : 'slate'}
          control="switch"
          footer={<StatusPill tone="slate">Ato will not install Podman automatically</StatusPill>}
        >
          Lets Ato use Podman as a container engine when a recipe needs one. If
          Podman or Docker Desktop is missing, Ato shows setup instructions
          instead of installing a container runtime.
        </ToggleCard>

        <p className="mt-2 text-[12px] font-bold tracking-widest text-slate-400 uppercase">
          Ato-managed language runtimes
        </p>
        <ToggleCard
          checked={nodeInstallEnabled}
          onToggle={() => setNodeInstallEnabled((v) => !v)}
          icon={Hexagon}
          title="Install Ato-managed Node.js"
          status={nodeInstallEnabled ? 'Will install when needed' : 'Disabled'}
          statusTone={nodeInstallEnabled ? 'violet' : 'slate'}
        >
          Uses Ato's toolchain cache instead of relying on a system Node.js, so
          JavaScript recipes run the same on every machine.
        </ToggleCard>

        <ToggleCard
          checked={uvInstallEnabled}
          onToggle={() => setUvInstallEnabled((v) => !v)}
          icon={Box}
          title="Install Ato-managed uv"
          status={uvInstallEnabled ? 'Will install when needed' : 'Disabled'}
          statusTone={uvInstallEnabled ? 'violet' : 'slate'}
        >
          Uses an Ato-supported uv from the toolchain cache for Python recipes
          that build with uv.
        </ToggleCard>

        <ToggleCard
          checked={pythonInstallEnabled}
          onToggle={() => setPythonInstallEnabled((v) => !v)}
          icon={FileCode}
          title="Install Ato-managed Python"
          status={pythonInstallEnabled ? 'Will install when needed' : 'Disabled'}
          statusTone={pythonInstallEnabled ? 'violet' : 'slate'}
        >
          Uses an Ato-supported Python from the toolchain cache for recipes that
          need Python.
        </ToggleCard>

        <div className="mt-2">
          <p className="mb-2 text-[12px] font-bold tracking-widest text-slate-400 uppercase">
            System checks
          </p>
          <div className="grid grid-cols-3 gap-3">
            <SystemCheck
              icon={Wrench}
              label="Ato helper"
              status="Bundled"
              tone="emerald"
              detail="Ships with Desktop."
            />
            <SystemCheck
              icon={ShipWheel}
              label="Nacelle"
              status="Bundled"
              tone="emerald"
              detail="Source runtime included."
            />
            <SystemCheck
              icon={Container}
              label="Docker Desktop"
              status="Detection only"
              tone="amber"
              detail="Ato checks it, but never installs it."
            />
          </div>
        </div>
      </div>

      <button
        onClick={onFinish}
        className="w-full py-4 bg-gradient-to-r from-[#A78BFA] to-[#8B5CF6] text-white rounded-2xl font-bold text-[17px] shadow-lg shadow-violet-500/25 hover:opacity-90 transition-opacity shrink-0 mt-6 flex justify-center items-center gap-2"
      >
        {ctaLabel} <span className="text-xl">→</span>
      </button>
    </div>
  )
}
