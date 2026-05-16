import React from 'react'
import { Box, Send, PlayCircle } from 'lucide-react'

export default function Step4({ onNext }) {
  return (
    <div className="flex flex-col h-full p-8">
      <div className="shrink-0">
        <div className="text-[#F43F5E] font-bold tracking-widest text-sm mb-4 mt-2">4 / 4</div>

        <div className="relative mb-2 inline-block w-fit">
          <h1 className="text-[40px] leading-tight font-extrabold text-[#0F172A] tracking-tight">Share your apps</h1>
          <div className="absolute -top-2 -right-12 w-12 h-12 pointer-events-none">
            <div className="absolute top-5 left-1 w-2.5 h-[3px] bg-[#F43F5E] rounded-full transform -rotate-[45deg]"></div>
            <div className="absolute top-1 left-4 w-4 h-[3px] bg-[#A78BFA] rounded-full transform -rotate-[15deg]"></div>
            <div className="absolute top-4 left-8 w-2.5 h-[3px] bg-[#FBBF24] rounded-full transform rotate-[20deg]"></div>
          </div>
        </div>

        <p className="text-[17px] text-slate-500 mb-8 pr-8 leading-relaxed">
          Pack your source code as a capsule,<br/>then share one link so others can run it.
        </p>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto">
        <div className="relative w-full h-[250px] mb-8 flex items-center justify-center overflow-hidden rounded-[24px]">
          <img
            src="/onboarding-step-4.png"
            alt="Share apps illustration"
            className="w-full h-full object-contain drop-shadow-sm"
            onError={(e) => {
              e.target.onerror = null
              e.target.src = "/onboarding-step-4.png"
            }}
          />
        </div>

        <div className="grid grid-cols-3 gap-3">
          <div className="bg-[#FFF1F2] border border-[#FFE4E6] rounded-[20px] p-4 flex flex-col items-start transition-all hover:shadow-md">
            <div className="flex items-center gap-2 mb-3 w-full">
              <div className="w-6 h-6 rounded-full bg-[#FECDD3] text-[#E11D48] flex items-center justify-center font-bold text-[13px]">1</div>
              <Box className="text-[#FB7185] ml-auto" size={24} strokeWidth={2} />
            </div>
            <h4 className="font-bold text-[#0F172A] text-[15px] mb-1">Pack</h4>
            <p className="text-[12px] text-slate-500 leading-snug">Turn your code into a portable capsule.</p>
          </div>

          <div className="bg-[#FFFBEB] border border-[#FEF3C7] rounded-[20px] p-4 flex flex-col items-start transition-all hover:shadow-md">
            <div className="flex items-center gap-2 mb-3 w-full">
              <div className="w-6 h-6 rounded-full bg-[#FDE68A] text-[#D97706] flex items-center justify-center font-bold text-[13px]">2</div>
              <Send className="text-[#FBBF24] ml-auto" size={22} strokeWidth={2} />
            </div>
            <h4 className="font-bold text-[#0F172A] text-[15px] mb-1">Share</h4>
            <p className="text-[12px] text-slate-500 leading-snug">Send one simple link to anyone.</p>
          </div>

          <div className="bg-[#ECFDF5] border border-[#D1FAE5] rounded-[20px] p-4 flex flex-col items-start transition-all hover:shadow-md">
            <div className="flex items-center gap-2 mb-3 w-full">
              <div className="w-6 h-6 rounded-full bg-[#A7F3D0] text-[#059669] flex items-center justify-center font-bold text-[13px]">3</div>
              <PlayCircle className="text-[#34D399] ml-auto" size={24} strokeWidth={2} />
            </div>
            <h4 className="font-bold text-[#0F172A] text-[15px] mb-1">Run</h4>
            <p className="text-[12px] text-slate-500 leading-snug">Recipients open it and run instantly.</p>
          </div>
        </div>
      </div>

      <button
        onClick={onNext}
        className="w-full py-4 bg-gradient-to-r from-[#FF905A] to-[#F43F5E] text-white rounded-2xl font-bold text-[17px] shadow-lg shadow-rose-500/25 hover:opacity-90 transition-opacity shrink-0 mt-6 flex justify-center items-center gap-2"
      >
        Get started <span className="text-xl">🎉</span>
      </button>
    </div>
  )
}
