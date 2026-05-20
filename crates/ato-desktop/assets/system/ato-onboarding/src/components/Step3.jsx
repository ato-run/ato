import React, { useState, useEffect } from 'react'
import { Check, Lock, Loader2, Home, Activity, Users, ArrowRightLeft, FileText } from 'lucide-react'

export default function Step3({ onNext }) {
  const [stage, setStage] = useState(0)
  const [isReady, setIsReady] = useState(false)

  useEffect(() => {
    const timers = [
      setTimeout(() => setStage(1), 800),
      setTimeout(() => setStage(2), 1800),
      setTimeout(() => setStage(3), 2800),
      setTimeout(() => {
        setStage(4)
        setIsReady(true)
      }, 3800),
    ]
    return () => timers.forEach(t => clearTimeout(t))
  }, [])

  return (
    <div className="flex flex-col h-full p-8 pb-6">
      <div className="shrink-0">
        <div className="text-[#F43F5E] font-bold tracking-widest text-sm mb-4 mt-2">3 / 4</div>
        <h1 className="text-[40px] leading-tight font-extrabold text-[#0F172A] mb-2 tracking-tight">Run it locally</h1>
        <p className="text-[17px] text-slate-500 mb-6 leading-relaxed">
          Ato detects the setup, prepares the environment,<br/>and opens a live preview.
        </p>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto">
        <div className="border border-slate-100 shadow-sm rounded-[24px] bg-white p-3.5 mb-6 flex items-center justify-between">
          <div className="flex items-center gap-4">
            <div className="w-[52px] h-[52px] bg-[#991B1B] rounded-[14px] flex items-center justify-center shadow-inner relative overflow-hidden">
              <div className="w-[26px] h-[26px] border-4 border-white rounded-[4px] transform rotate-45"></div>
              <div className="absolute inset-0 bg-gradient-to-br from-white/20 to-transparent"></div>
            </div>
            <div className="flex flex-col">
              <span className="text-slate-500 text-[13px] font-medium leading-tight">Demo app</span>
              <span className="text-[#0F172A] font-bold text-xl leading-tight mt-0.5">WasedaP2P</span>
            </div>
          </div>
          <div className={`flex items-center gap-2 px-3 py-1 rounded-full text-sm font-medium transition-all duration-500 ${isReady ? 'text-[#10B981] bg-emerald-50' : 'text-slate-400 bg-slate-100'}`}>
            {isReady ? (
              <>
                <div className="w-2 h-2 rounded-full bg-[#10B981] animate-pulse"></div>
                Running
              </>
            ) : (
              <>
                <Loader2 className="w-3.5 h-3.5 animate-spin" />
                Launching...
              </>
            )}
          </div>
        </div>

        <div className="px-2 mb-8">
          <div className="flex items-start justify-between relative z-0">
            <div className="absolute top-[15px] left-6 right-6 h-[3px] bg-slate-100 -z-10 rounded-full"></div>
            <div
              className="absolute top-[15px] left-6 h-[3px] bg-gradient-to-r from-[#10B981] via-[#10B981] to-[#8B5CF6] -z-10 rounded-full transition-all duration-1000 ease-in-out"
              style={{ width: `${Math.max(0, (stage - 1) * 33)}%` }}
            ></div>

            {[
              { label: "Detect\ndependencies", stepNum: 1 },
              { label: "Prepare\nruntime", stepNum: 2 },
              { label: "Launch\napp", stepNum: 3 },
              { label: "Open\nlive preview", stepNum: 4 },
            ].map((s, i) => {
              const isDone = stage > i
              const isActive = stage === i + 1
              return (
                <div key={i} className="flex flex-col items-center gap-2.5 w-[76px]">
                  <div className={`w-[30px] h-[30px] rounded-full border-[2.5px] flex items-center justify-center bg-white transition-all duration-500
                    ${isDone ? 'border-[#10B981] text-[#10B981] scale-110' : isActive ? 'border-[#8B5CF6] text-[#8B5CF6] shadow-[0_0_10px_rgba(139,92,246,0.3)]' : 'border-slate-200 text-slate-300'}`}>
                    {isDone ? <Check size={16} strokeWidth={3.5} /> : <span className="font-bold text-[13px]">{s.stepNum}</span>}
                  </div>
                  <span className={`text-[11px] leading-tight text-center whitespace-pre-wrap font-medium transition-colors duration-500
                    ${isDone ? 'text-slate-800' : isActive ? 'text-[#8B5CF6]' : 'text-slate-400'}`}>
                    {s.label}
                  </span>
                </div>
              )
            })}
          </div>
        </div>

        <div className="relative mb-2" style={{ minHeight: "200px" }}>
          <div className={`w-full h-full min-h-[200px] bg-white border border-slate-200 rounded-[16px] shadow-xl overflow-hidden flex flex-col ring-[6px] ring-slate-50 transition-all duration-700
            ${isReady ? 'opacity-100 translate-y-0 scale-100' : 'opacity-0 translate-y-10 scale-95'}`}>

            <div className="h-9 bg-[#F8FAFC] border-b border-slate-200 flex items-center justify-between px-3 relative shrink-0">
              <div className="flex gap-1.5">
                <div className="w-2.5 h-2.5 rounded-full bg-[#F87171]"></div>
                <div className="w-2.5 h-2.5 rounded-full bg-[#FBBF24]"></div>
                <div className="w-2.5 h-2.5 rounded-full bg-[#34D399]"></div>
              </div>
              <div className="absolute left-1/2 -translate-x-1/2 w-48 h-5 bg-white rounded-md border border-slate-200 shadow-sm"></div>
              <div className="flex items-center gap-1.5 bg-emerald-100/80 text-emerald-700 px-2.5 py-0.5 rounded-full text-[10px] font-bold">
                <div className="w-1.5 h-1.5 rounded-full bg-emerald-500 animate-pulse"></div>
                Live
              </div>
            </div>

            <div className="flex flex-1 overflow-hidden">
              <div className="w-[110px] bg-[#1E1B4B] flex flex-col p-3 shrink-0">
                <div className="flex items-center gap-2 text-white font-bold text-xs mb-5 mt-1">
                  <div className="w-4 h-4 bg-white/20 rounded-sm flex items-center justify-center">
                    <div className="w-2 h-2 border border-white transform rotate-45"></div>
                  </div>
                  WasedaP2P
                </div>
                <div className="flex flex-col gap-1 flex-1 text-[10px] font-medium text-slate-300">
                  <div className="bg-[#4F46E5] text-white rounded-[6px] px-2 py-1.5 flex items-center gap-1.5 shadow-sm"><Home size={12}/> Overview</div>
                  <div className="px-2 py-1.5 flex items-center gap-1.5 hover:bg-white/10 rounded-[6px]"><Activity size={12}/> Activity</div>
                  <div className="px-2 py-1.5 flex items-center gap-1.5 hover:bg-white/10 rounded-[6px]"><Users size={12}/> Peers</div>
                  <div className="px-2 py-1.5 flex items-center gap-1.5 hover:bg-white/10 rounded-[6px]"><ArrowRightLeft size={12}/> Transfers</div>
                  <div className="px-2 py-1.5 flex items-center gap-1.5 hover:bg-white/10 rounded-[6px]"><FileText size={12}/> Files</div>
                </div>
              </div>
              <div className="flex-1 bg-[#F8FAFC] p-4 overflow-hidden flex flex-col">
                <h2 className="text-sm font-bold text-[#0F172A] mb-0.5">Overview</h2>
                <div className="w-24 h-1.5 bg-slate-200 rounded-full mb-4"></div>

                <div className="grid grid-cols-2 gap-2 mb-4">
                  {[1, 2].map(i => (
                    <div key={i} className="bg-white rounded-lg border border-slate-100 p-2 shadow-sm">
                      <div className="w-10 h-1.5 bg-slate-100 rounded-full mb-1.5"></div>
                      <div className="w-16 h-4 bg-slate-50 rounded mt-1"></div>
                    </div>
                  ))}
                </div>

                <div className="flex-1 bg-white rounded-xl border border-slate-100 p-3 shadow-sm flex flex-col">
                  <div className="w-20 h-2 bg-slate-100 rounded-full mb-3"></div>
                  <div className="flex-1 flex items-end gap-1 px-1">
                    {[40, 70, 45, 90, 65, 80, 50].map((h, i) => (
                      <div key={i} className="flex-1 bg-indigo-100 rounded-t-sm" style={{ height: `${h}%` }}></div>
                    ))}
                  </div>
                </div>
              </div>
            </div>
          </div>

          {!isReady && (
            <div className="absolute inset-0 flex flex-col items-center justify-center bg-slate-50/50 rounded-[16px] border-2 border-dashed border-slate-200">
              <div className="relative">
                <Loader2 className="w-10 h-10 text-slate-300 animate-spin" />
                <div className="absolute inset-0 flex items-center justify-center">
                  <div className="w-1.5 h-1.5 rounded-full bg-slate-300"></div>
                </div>
              </div>
              <p className="text-xs text-slate-400 font-bold mt-3 uppercase tracking-widest">Environment Setup</p>
            </div>
          )}
        </div>
      </div>

      <div className="shrink-0 mt-3">
        <div className="flex items-center justify-center gap-1.5 text-slate-400 text-[13px] mb-2">
          <Lock size={14} />
          <span>Runs locally in a controlled environment.</span>
        </div>

        <button
          onClick={onNext}
          disabled={!isReady}
          className={`w-full py-4 rounded-2xl font-bold text-[17px] shadow-lg transition-all duration-300 ${isReady ? 'bg-gradient-to-r from-[#FF905A] to-[#F43F5E] text-white shadow-rose-500/25 cursor-pointer' : 'bg-slate-200 text-slate-400 shadow-none cursor-not-allowed'}`}
        >
          {isReady ? 'Continue' : 'Preparing...'}
        </button>
      </div>
    </div>
  )
}
