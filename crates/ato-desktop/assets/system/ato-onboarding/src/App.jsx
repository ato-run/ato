import { useState, useEffect } from 'react'
import OrbitBackground from './components/OrbitBackground'
import Step1 from './components/Step1'
import Step2 from './components/Step2'
import Step3 from './components/Step3'
import Step4 from './components/Step4'
import { BRIDGE } from './bridge'

const ONBOARDING_VERSION = 1

const orbitColors = {
  1: 'rgba(37, 99, 235, 0.25)',
  2: 'rgba(244, 63, 94, 0.25)',
  3: 'rgba(139, 92, 246, 0.25)',
  4: 'rgba(251, 191, 36, 0.35)',
}

export default function App() {
  const [step, setStep] = useState(1)

  const nextStep = () => setStep((prev) => Math.min(prev + 1, 4))
  const prevStep = () => setStep((prev) => Math.max(prev - 1, 1))
  const finish = () => BRIDGE({ kind: "complete", version: ONBOARDING_VERSION, skipped: false })

  useEffect(() => {
    const onKey = (e) => {
      if (e.key === "Enter" || e.key === "ArrowRight") {
        e.preventDefault()
        if (step === 4) finish()
        else nextStep()
      } else if (e.key === "ArrowLeft") {
        e.preventDefault()
        prevStep()
      } else if (e.key === "Escape") {
        e.preventDefault()
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
          {step === 4 && <Step4 onNext={finish} />}
        </div>
      </div>
    </div>
  )
}
