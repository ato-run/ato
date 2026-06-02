import React from 'react'
import { ShieldCheck, Box, Hexagon, FileCode, Container } from 'lucide-react'

// Runtime Setup (issue #420 revision): instead of scanning host devices, Ato
// checks the *runtime tools* recipes need and installs Ato-managed language
// runtimes when required. These toggles are default-on, opt-out preferences.
// State is owned by App so the keyboard "finish" path and the button submit the
// same values.
function ToggleCard({ checked, onToggle, icon: Icon, title, children }) {
  return (
    <button
      type="button"
      onClick={onToggle}
      className={`w-full text-left rounded-[20px] border p-4 flex gap-3 items-start transition-all ${
        checked ? 'bg-[#F5F3FF] border-[#DDD6FE]' : 'bg-white border-slate-200'
      }`}
    >
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
      <span className="min-w-0">
        <span className="flex items-center gap-2 mb-1">
          <Icon className="text-[#8B5CF6]" size={18} strokeWidth={2} />
          <span className="font-bold text-[#0F172A] text-[15px]">{title}</span>
        </span>
        <span className="block text-[13px] text-slate-500 leading-snug">{children}</span>
      </span>
    </button>
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
          Ato checks the tools a recipe needs to run on this machine, and can
          install its own managed copies of the language runtimes so launches are
          reproducible. These are on by default — change them later in Settings.
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
          title="Use Podman for containers"
        >
          Lets Ato use Podman as a container engine when a recipe needs one. Ato
          does not install Podman or Docker Desktop automatically — if neither is
          available, Ato shows setup instructions. Turn this off to keep Ato away
          from Podman entirely.
        </ToggleCard>

        <p className="mt-2 text-[12px] font-bold tracking-widest text-slate-400 uppercase">
          Ato-managed language runtimes
        </p>
        <ToggleCard
          checked={nodeInstallEnabled}
          onToggle={() => setNodeInstallEnabled((v) => !v)}
          icon={Hexagon}
          title="Install Ato-managed Node.js when needed"
        >
          Installs an Ato-supported Node.js into Ato's toolchain cache, instead of
          relying on a system Node, so recipes run the same on every machine.
        </ToggleCard>

        <ToggleCard
          checked={uvInstallEnabled}
          onToggle={() => setUvInstallEnabled((v) => !v)}
          icon={Box}
          title="Install Ato-managed uv when needed"
        >
          Installs an Ato-supported uv into Ato's toolchain cache for Python
          recipes that build with uv.
        </ToggleCard>

        <ToggleCard
          checked={pythonInstallEnabled}
          onToggle={() => setPythonInstallEnabled((v) => !v)}
          icon={FileCode}
          title="Install Ato-managed Python when needed"
        >
          Installs an Ato-supported Python into Ato's toolchain cache for recipes
          that need it.
        </ToggleCard>
      </div>

      <button
        onClick={onFinish}
        className="w-full py-4 bg-gradient-to-r from-[#A78BFA] to-[#8B5CF6] text-white rounded-2xl font-bold text-[17px] shadow-lg shadow-violet-500/25 hover:opacity-90 transition-opacity shrink-0 mt-6 flex justify-center items-center gap-2"
      >
        Get started <span className="text-xl">🎉</span>
      </button>
    </div>
  )
}
