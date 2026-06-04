import { useState, useEffect } from 'react'
import OrbitBackground from './components/OrbitBackground'
import Step1 from './components/Step1'
import Step2 from './components/Step2'
import Step3 from './components/Step3'
import Step4 from './components/Step4'
import Step5 from './components/Step5'
import { BRIDGE } from './bridge'

const ONBOARDING_VERSION = 1
const LAST_STEP = 5

const orbitColors = {
  1: 'rgba(37, 99, 235, 0.25)',
  2: 'rgba(244, 63, 94, 0.25)',
  3: 'rgba(139, 92, 246, 0.25)',
  4: 'rgba(251, 191, 36, 0.35)',
  5: 'rgba(139, 92, 246, 0.30)',
}

export default function App() {
  const [step, setStep] = useState(1)
  // Runtime-setup preferences. Default on (opt-out); state lives here so the
  // keyboard "finish" path and the Step 5 button submit the same values.
  // Podman governs OCI provider selection; the language-tool toggles control
  // whether Ato may install its own managed Node/uv/Python when a recipe needs
  // them (managed-first policy).
  const [podmanEnabled, setPodmanEnabled] = useState(true)
  const [nodeInstallEnabled, setNodeInstallEnabled] = useState(true)
  const [uvInstallEnabled, setUvInstallEnabled] = useState(true)
  const [pythonInstallEnabled, setPythonInstallEnabled] = useState(true)

  const nextStep = () => setStep((prev) => Math.min(prev + 1, LAST_STEP))
  const prevStep = () => setStep((prev) => Math.max(prev - 1, 1))

  // Persist the runtime-setup choices, then mark onboarding complete. The save
  // command is sent first so the toggles land in desktop config before the
  // complete command tears down the window. `overrides` lets the Step 5 "Skip
  // Podman for now" path persist `podman_enabled: false` deterministically
  // without waiting on an async setState to flush.
  const finish = (overrides = {}, launchHandle = null) => {
    BRIDGE({
      kind: "save_runtime_setup_settings",
      podman_enabled: podmanEnabled,
      node_install_enabled: nodeInstallEnabled,
      uv_install_enabled: uvInstallEnabled,
      python_install_enabled: pythonInstallEnabled,
      ...overrides,
    })
    // #460 PR3: `launchHandle` resumes straight into a sample capsule after
    // onboarding closes ("Continue to sample app"); omitted → startup surface.
    BRIDGE({
      kind: "complete",
      version: ONBOARDING_VERSION,
      skipped: false,
      ...(launchHandle ? { launch_handle: launchHandle } : {}),
    })
  }

  useEffect(() => {
    const onKey = (e) => {
      if (e.key === "Enter" || e.key === "ArrowRight") {
        // On the final step, Step 5 owns the primary action — which may need to
        // run a runtime install *before* completing. Finishing here on Enter
        // would silently skip that install (the bug this guard fixes), so we
        // let Step 5's own key handler route Enter through `handlePrimary`.
        if (step === LAST_STEP) return
        e.preventDefault()
        nextStep()
      } else if (e.key === "ArrowLeft") {
        e.preventDefault()
        prevStep()
      } else if (e.key === "Escape") {
        e.preventDefault()
        // Skipping leaves all runtime-setup settings at their default (on).
        BRIDGE({ kind: "complete", version: ONBOARDING_VERSION, skipped: true })
      }
    }
    document.addEventListener("keydown", onKey)
    return () => document.removeEventListener("keydown", onKey)
  }, [step])

  return (
    <div className="w-screen h-screen bg-slate-100 font-sans selection:bg-rose-100 selection:text-rose-900">
      <div className="w-full h-full bg-[#F8FAFC] overflow-hidden relative">
        <OrbitBackground color={orbitColors[step]} />
        <div key={step} className="w-full h-full relative z-10 animate-[fadeIn_0.4s_ease-out]">
          {step === 1 && <Step1 onNext={nextStep} />}
          {step === 2 && <Step2 onNext={nextStep} />}
          {step === 3 && <Step3 onNext={nextStep} />}
          {step === 4 && <Step4 onNext={nextStep} />}
          {step === 5 && (
            <Step5
              onFinish={finish}
              podmanEnabled={podmanEnabled}
              setPodmanEnabled={setPodmanEnabled}
              nodeInstallEnabled={nodeInstallEnabled}
              setNodeInstallEnabled={setNodeInstallEnabled}
              uvInstallEnabled={uvInstallEnabled}
              setUvInstallEnabled={setUvInstallEnabled}
              pythonInstallEnabled={pythonInstallEnabled}
              setPythonInstallEnabled={setPythonInstallEnabled}
            />
          )}
        </div>
      </div>
    </div>
  )
}
