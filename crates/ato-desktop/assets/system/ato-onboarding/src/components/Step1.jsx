import React from 'react'

export default function Step1({ onNext }) {
  return (
    <div className="flex flex-col h-full p-8 relative">
      <div className="shrink-0 relative z-10">
        <div className="text-blue-600 font-bold tracking-widest text-sm mb-6 mt-4">1 / 4</div>
        <h1 className="text-[40px] leading-tight font-extrabold text-[#0F172A] mb-3 tracking-tight">Welcome to Ato</h1>
        <p className="text-lg text-slate-500">Run apps without manual setup.</p>
      </div>

      <div className="flex-1 flex items-center justify-center min-h-0 overflow-y-auto relative z-10">
        <div className="flex items-center gap-5 -mt-12">
          <svg width="72" height="72" viewBox="0 0 48 48" fill="none">
            <path d="M24 4L8 40H20.5L24 30L27.5 40H40L24 4Z" fill="#2563EB"/>
            <circle cx="24" cy="30" r="5" fill="#60A5FA"/>
          </svg>
          <span className="text-6xl font-bold text-[#0F172A] tracking-tight">Ato</span>
        </div>
      </div>

      <button
        onClick={onNext}
        className="w-full py-4 bg-gradient-to-r from-[#4A86FF] to-[#2B60FF] text-white rounded-2xl font-bold text-[17px] shadow-lg shadow-blue-500/25 hover:opacity-90 transition-opacity shrink-0 relative z-10"
      >
        Get started
      </button>
    </div>
  )
}
