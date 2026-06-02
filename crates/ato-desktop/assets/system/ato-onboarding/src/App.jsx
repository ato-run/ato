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
  // Runtime-safety opt-out toggles. Default on (opt-out); state lives here so
  // the keyboard "finish" path and the Step 5 button submit the same values.
  const [podmanEnabled, setPodmanEnabled] = useState(true)
  const [hostDetectionEnabled, setHostDetectionEnabled] = useState(true)

  const nextStep = () => setStep((prev) => Math.min(prev + 1, LAST_STEP))
  const prevStep = () => setStep((prev) => Math.max(prev - 1, 1))

  // Persist the runtime-safety choices, then mark onboarding complete. The
  // save command is sent first so the toggles land in desktop config before
  // the complete command tears down the window.
  const finish = () => {
    BRIDGE({
      kind: "save_runtime_optout_settings",
      podman_enabled: podmanEnabled,
      host_device_detection_enabled: hostDetectionEnabled,
    })
    BRIDGE({ kind: "complete", version: ONBOARDING_VERSION, skipped: false })
  }

  useEffect(() => {
    const onKey = (e) => {
      if (e.key === "Enter" || e.key === "ArrowRight") {
        e.preventDefault()
        if (step === LAST_STEP) finish()
        else nextStep()
      } else if (e.key === "ArrowLeft") {
        e.preventDefault()
        prevStep()
      } else if (e.key === "Escape") {
        e.preventDefault()
        // Skipping leaves both runtime-safety settings at their default (on).
        BRIDGE({ kind: "complete", version: ONBOARDING_VERSION, skipped: true })
      }
    }
    document.addEventListener("keydown", onKey)
    return () => document.removeEventListener("keydown", onKey)
  }, [step, podmanEnabled, hostDetectionEnabled])

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
              hostDetectionEnabled={hostDetectionEnabled}
              setHostDetectionEnabled={setHostDetectionEnabled}
            />
          )}
        </div>
      </div>
    </div>
  )
}
