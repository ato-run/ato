import React from 'react'
import { Box, Activity, Smile, FileText, Github, Folder, ChevronDown } from 'lucide-react'

export default function Step2({ onNext }) {
  return (
    <div className="flex flex-col h-full p-8">
      <div className="shrink-0">
        <div className="text-[#F43F5E] font-bold tracking-widest text-sm mb-4 mt-2">2 / 4</div>
        <h1 className="text-[40px] leading-tight font-extrabold text-[#0F172A] mb-2 tracking-tight">Choose what to run</h1>
        <p className="text-[17px] text-slate-500 mb-6 leading-relaxed">
          Start from a featured app, a GitHub repo,<br/>or a local folder.
        </p>
      </div>

      <div className="flex-1 flex flex-col gap-3 min-h-0 overflow-y-auto">
        <div className="border-2 border-rose-200 bg-white rounded-3xl p-4 flex flex-col shadow-sm">
          <div className="flex items-start justify-between mb-3">
            <div>
              <h3 className="text-lg font-bold text-[#0F172A]">Featured apps</h3>
              <p className="text-slate-500 text-[13px] leading-tight mt-0.5">Curated apps ready to run.</p>
            </div>
            <button className="px-3 py-1.5 text-[12px] font-bold text-[#F43F5E] bg-rose-50 hover:bg-rose-100 rounded-lg transition-colors">
              Browse all
            </button>
          </div>

          <div className="grid grid-cols-2 gap-2 mt-1">
            <div className="flex items-center gap-2.5 p-2 rounded-xl hover:bg-slate-50 border border-slate-100 cursor-pointer transition-colors">
              <div className="w-9 h-9 rounded-lg bg-blue-100 text-blue-600 flex items-center justify-center shrink-0">
                <Box size={18}/>
              </div>
              <div className="flex flex-col overflow-hidden">
                <span className="text-[13px] font-bold text-slate-700 truncate">WasedaP2P</span>
                <span className="text-[11px] text-slate-500 truncate">P2P Network</span>
              </div>
            </div>
            <div className="flex items-center gap-2.5 p-2 rounded-xl hover:bg-slate-50 border border-slate-100 cursor-pointer transition-colors">
              <div className="w-9 h-9 rounded-lg bg-emerald-100 text-emerald-600 flex items-center justify-center shrink-0">
                <Activity size={18}/>
              </div>
              <div className="flex flex-col overflow-hidden">
                <span className="text-[13px] font-bold text-slate-700 truncate">Dashboard</span>
                <span className="text-[11px] text-slate-500 truncate">Analytics</span>
              </div>
            </div>
            <div className="flex items-center gap-2.5 p-2 rounded-xl hover:bg-slate-50 border border-slate-100 cursor-pointer transition-colors">
              <div className="w-9 h-9 rounded-lg bg-purple-100 text-purple-600 flex items-center justify-center shrink-0">
                <Smile size={18}/>
              </div>
              <div className="flex flex-col overflow-hidden">
                <span className="text-[13px] font-bold text-slate-700 truncate">Chat App</span>
                <span className="text-[11px] text-slate-500 truncate">Realtime</span>
              </div>
            </div>
            <div className="flex items-center gap-2.5 p-2 rounded-xl hover:bg-slate-50 border border-slate-100 cursor-pointer transition-colors">
              <div className="w-9 h-9 rounded-lg bg-amber-100 text-amber-600 flex items-center justify-center shrink-0">
                <FileText size={18}/>
              </div>
              <div className="flex flex-col overflow-hidden">
                <span className="text-[13px] font-bold text-slate-700 truncate">Blog Template</span>
                <span className="text-[11px] text-slate-500 truncate">Static site</span>
              </div>
            </div>
          </div>
        </div>

        <div className="border border-slate-200 bg-white rounded-[20px] p-3 flex items-center gap-4 transition-all shadow-sm hover:shadow-md hover:border-slate-300 cursor-pointer group mt-1">
          <div className="w-12 h-12 bg-slate-50 rounded-xl flex items-center justify-center shrink-0 border border-slate-100 group-hover:bg-slate-100 transition-colors">
            <Github className="w-6 h-6 text-[#0F172A]" />
          </div>
          <div className="flex-1">
            <h3 className="text-[15px] font-bold text-[#0F172A]">GitHub repo</h3>
            <p className="text-slate-500 text-[13px] leading-tight mt-0.5">Run a repository with minimal setup.</p>
          </div>
          <ChevronDown className="w-5 h-5 text-slate-400 transform -rotate-90 group-hover:text-slate-600 group-hover:translate-x-1 transition-all" />
        </div>

        <div className="border border-slate-200 bg-white rounded-[20px] p-3 flex items-center gap-4 transition-all shadow-sm hover:shadow-md hover:border-amber-200 cursor-pointer group">
          <div className="w-12 h-12 bg-[#FEF3C7] rounded-xl flex items-center justify-center shrink-0 border border-amber-100 group-hover:bg-amber-200 transition-colors">
            <Folder className="w-6 h-6 text-[#F59E0B]" fill="currentColor" />
          </div>
          <div className="flex-1">
            <h3 className="text-[15px] font-bold text-[#0F172A]">Local folder</h3>
            <p className="text-slate-500 text-[13px] leading-tight mt-0.5">Open code already on your machine.</p>
          </div>
          <ChevronDown className="w-5 h-5 text-slate-400 transform -rotate-90 group-hover:text-slate-600 group-hover:translate-x-1 transition-all" />
        </div>
      </div>

      <button
        onClick={onNext}
        className="w-full py-4 bg-gradient-to-r from-[#FF905A] to-[#F43F5E] text-white rounded-2xl font-bold text-[17px] shadow-lg shadow-rose-500/25 hover:opacity-90 transition-opacity shrink-0"
      >
        Continue
      </button>
    </div>
  )
}
