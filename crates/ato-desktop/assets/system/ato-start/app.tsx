import React from 'react';
import { 
  Search, Github, Folder, Terminal, ShoppingBag, 
  Clock, Rocket, Users, Settings, Sparkles, Star, 
  Play, ArrowRight, Box, Activity, Store
} from 'lucide-react';

function postIpc(message: unknown): boolean {
  const w = window as unknown as {
    __ATO_IPC__?: { postMessage: (m: string) => void };
    ipc?: { postMessage: (m: string) => void };
  };
  const bridge = w.__ATO_IPC__ ?? w.ipc;
  if (!bridge || typeof bridge.postMessage !== 'function') {
    console.log('[ato-start bridge missing]', message);
    return false;
  }
  bridge.postMessage(JSON.stringify(message));
  return true;
}

function submitQuery(value: string) {
  const trimmed = value.trim();
  if (trimmed.length === 0) return;
  // Route through ato-start's OpenQuery dispatch, which already
  // classifies the input and forwards GitHub repo inputs to the
  // GitHub Import review surface.
  postIpc({ capsule: 'ato-start', command: { kind: 'open_query', value: trimmed } });
}

export default function App() {
  const [query, setQuery] = React.useState('');
  return (
    <div className="min-h-screen bg-slate-50 flex items-center justify-center p-4 sm:p-8 font-sans selection:bg-rose-100 selection:text-rose-900">

      {/* Main App Window */}
      <div className="w-full max-w-[1024px] h-[740px] bg-white rounded-2xl shadow-2xl overflow-hidden relative flex flex-col ring-1 ring-slate-900/5">
        
        {/* Animated Background */}
        <BackgroundDecorations />

        {/* Mac-style Header */}
        <div className="h-10 flex items-center px-4 gap-2 absolute top-0 left-0 w-full z-20">
          <div className="w-3 h-3 rounded-full bg-[#FF5F56]"></div>
          <div className="w-3 h-3 rounded-full bg-[#FFBD2E]"></div>
          <div className="w-3 h-3 rounded-full bg-[#27C93F]"></div>
        </div>

        {/* Content Area */}
        <div className="flex-1 overflow-y-auto relative z-10 px-8 pt-10 pb-20 custom-scrollbar">
          
          {/* Top Section: Logo & Search */}
          <div className="flex flex-col items-center pt-8">
            <div className="w-14 h-14 bg-gradient-to-br from-rose-400 to-rose-500 rounded-2xl flex items-center justify-center text-white font-bold text-3xl shadow-lg shadow-rose-500/30 mb-4">
              A
            </div>
            <h2 className="text-[28px] font-extrabold text-[#0F172A] mb-1 tracking-tight">Start from Source</h2>
            <p className="text-[13px] text-slate-500 mb-8 font-medium">Run a repo, local app, or capsule without setup</p>
            
            {/* Search Bar */}
            <div className="w-full max-w-[640px] flex items-center gap-2 px-4 py-3 bg-white border border-rose-200 rounded-full shadow-[0_4px_20px_rgba(244,63,94,0.06)] mb-4 transition-shadow focus-within:shadow-[0_4px_25px_rgba(244,63,94,0.12)] focus-within:border-rose-300">
              <Search size={18} className="text-slate-400" />
              <input
                type="text"
                placeholder="GitHub repo, local path, capsule, URL, or command"
                className="flex-1 outline-none text-[13px] text-slate-700 placeholder:text-slate-400 font-medium bg-transparent"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    submitQuery(query);
                  }
                }}
              />
              <kbd className="text-[10px] font-sans font-medium text-slate-400 bg-slate-50 border border-slate-200 px-1.5 py-0.5 rounded">⌘K</kbd>
            </div>

            {/* Suggestions */}
            <div className="flex flex-wrap items-center justify-center gap-2.5 mb-8">
              <div className="flex items-center gap-1.5 px-3 py-1.5 rounded-full border border-slate-200 bg-white text-[11px] text-slate-600 font-medium cursor-pointer hover:bg-slate-50 shadow-sm transition-colors">
                <Github size={12} className="text-slate-700"/> github.com/owner/repo
              </div>
              <div className="flex items-center gap-1.5 px-3 py-1.5 rounded-full border border-slate-200 bg-white text-[11px] text-slate-600 font-medium cursor-pointer hover:bg-slate-50 shadow-sm transition-colors">
                <Folder size={12} className="text-slate-400"/> ~/my-project
              </div>
              <div className="flex items-center gap-1.5 px-3 py-1.5 rounded-full border border-slate-200 bg-white text-[11px] text-slate-600 font-medium cursor-pointer hover:bg-slate-50 shadow-sm transition-colors">
                <Activity size={12} className="text-purple-400"/> capsule://...
              </div>
              <div className="flex items-center gap-1.5 px-3 py-1.5 rounded-full border border-slate-200 bg-white text-[11px] text-slate-600 font-medium cursor-pointer hover:bg-slate-50 shadow-sm transition-colors">
                <div className="bg-slate-700 text-white p-[3px] rounded-sm"><Terminal size={10} strokeWidth={3} /></div> python app.py
              </div>
            </div>

            {/* Tabs */}
            <div className="flex items-center justify-center gap-8 mb-8">
              <div className="flex items-center gap-1.5 text-[13px] font-bold text-blue-500 cursor-pointer">
                <div className="w-6 h-6 bg-blue-50 rounded-md flex items-center justify-center"><Store size={14} strokeWidth={2.5}/></div>
                Store
              </div>
              <div className="flex items-center gap-1.5 text-[13px] font-bold text-slate-700 cursor-pointer hover:text-slate-900 transition-colors">
                <Github size={16}/> GitHub
              </div>
              <div className="flex items-center gap-1.5 text-[13px] font-bold text-amber-500 cursor-pointer hover:text-amber-600 transition-colors">
                <Folder size={16} fill="currentColor" className="text-amber-500"/> Local
              </div>
              <div className="flex items-center gap-1.5 text-[13px] font-bold text-purple-400 cursor-pointer hover:text-purple-500 transition-colors">
                <Clock size={16}/> Recent
              </div>
            </div>
          </div>

          {/* Action Cards Grid */}
          <div className="grid grid-cols-4 gap-4 mb-8">
            {/* Run any repo */}
            <div className="flex flex-col p-4 rounded-2xl border border-rose-100 bg-rose-50/30 shadow-sm cursor-pointer hover:shadow-md transition-all hover:-translate-y-0.5 group">
              <div className="flex items-start gap-3 mb-4">
                <div className="w-12 h-12 rounded-[14px] bg-gradient-to-br from-rose-400 to-rose-500 text-white flex items-center justify-center shadow-inner shrink-0 group-hover:scale-105 transition-transform">
                  <Rocket size={20}/>
                </div>
                <div className="mt-1">
                  <h3 className="font-bold text-[14px] text-[#0F172A]">Run any repo</h3>
                  <p className="text-[11px] text-slate-500 leading-tight mt-0.5">Launch directly from source</p>
                </div>
              </div>
              <div className="mt-auto">
                <div className="inline-flex items-center gap-1 text-[10px] font-bold text-slate-600 bg-white border border-slate-200 px-2 py-1 rounded-md shadow-sm">
                  <Github size={12}/> GitHub
                </div>
              </div>
            </div>
            
            {/* Open local app */}
            <div className="flex flex-col p-4 rounded-2xl border border-amber-100 bg-amber-50/30 shadow-sm cursor-pointer hover:shadow-md transition-all hover:-translate-y-0.5 group">
              <div className="flex items-start gap-3 mb-4">
                <div className="w-12 h-12 rounded-[14px] bg-gradient-to-br from-amber-400 to-orange-400 text-white flex items-center justify-center shadow-inner shrink-0 group-hover:scale-105 transition-transform">
                  <Folder size={20} fill="currentColor"/>
                </div>
                <div className="mt-1">
                  <h3 className="font-bold text-[14px] text-[#0F172A]">Open local app</h3>
                  <p className="text-[11px] text-slate-500 leading-tight mt-0.5">Run a folder, file, or project</p>
                </div>
              </div>
              <div className="mt-auto">
                <div className="inline-flex items-center gap-1 text-[10px] font-bold text-slate-600 bg-white border border-slate-200 px-2 py-1 rounded-md shadow-sm">
                  <Folder size={12} className="text-amber-500" fill="currentColor"/> Local
                </div>
              </div>
            </div>

            {/* Browse store */}
            <div className="flex flex-col p-4 rounded-2xl border border-blue-100 bg-blue-50/30 shadow-sm cursor-pointer hover:shadow-md transition-all hover:-translate-y-0.5 group">
              <div className="flex items-start gap-3 mb-4">
                <div className="w-12 h-12 rounded-[14px] bg-gradient-to-br from-blue-400 to-blue-500 text-white flex items-center justify-center shadow-inner shrink-0 group-hover:scale-105 transition-transform">
                  <ShoppingBag size={20}/>
                </div>
                <div className="mt-1">
                  <h3 className="font-bold text-[14px] text-[#0F172A]">Browse store</h3>
                  <p className="text-[11px] text-slate-500 leading-tight mt-0.5">Discover apps and capsules</p>
                </div>
              </div>
              <div className="mt-auto">
                <div className="inline-flex items-center gap-1 text-[10px] font-bold text-blue-600 bg-blue-50 border border-blue-100 px-2 py-1 rounded-md shadow-sm">
                  <Store size={12}/> Store
                </div>
              </div>
            </div>

            {/* New workspace */}
            <div className="flex flex-col p-4 rounded-2xl border border-slate-100 bg-slate-50/80 shadow-sm cursor-pointer hover:shadow-md transition-all hover:-translate-y-0.5 group">
              <div className="flex items-start gap-3 mb-4">
                <div className="w-12 h-12 rounded-[14px] bg-slate-100 text-slate-400 flex items-center justify-center border border-slate-200 shrink-0 group-hover:bg-slate-200 transition-colors">
                  <Users size={20}/>
                </div>
                <div className="mt-1">
                  <h3 className="font-bold text-[14px] text-[#0F172A]">New workspace</h3>
                  <p className="text-[11px] text-slate-500 leading-tight mt-0.5">Optional</p>
                </div>
              </div>
              <div className="mt-auto">
                <div className="inline-flex items-center gap-1 text-[10px] font-bold text-purple-500 bg-purple-50 border border-purple-100 px-2 py-1 rounded-md shadow-sm">
                  <Clock size={12}/> Coming soon
                </div>
              </div>
            </div>
          </div>

          {/* Empty States Grid */}
          <div className="grid grid-cols-2 gap-6 mb-8">
            {/* Recent Capsules */}
            <div className="flex flex-col gap-3">
              <div className="flex items-center justify-between px-1">
                <div className="flex items-center gap-2 text-[13px] font-bold text-slate-800">
                  <Clock size={16} className="text-purple-500" /> Recent Capsules
                </div>
                <a href="#" className="text-[11px] font-bold text-rose-500 hover:text-rose-600 transition-colors">Show all</a>
              </div>
              <div className="h-28 border border-rose-50 bg-[#FCFDFE] rounded-2xl flex items-center justify-center gap-4 p-4 shadow-[inset_0_0_20px_rgba(244,63,94,0.02)]">
                <div className="w-12 h-12 bg-white rounded-full flex items-center justify-center shadow-sm border border-slate-100 text-slate-300 shrink-0">
                  <Box size={20} />
                </div>
                <div className="flex flex-col">
                  <h4 className="text-[13px] font-bold text-slate-700">No capsules launched yet</h4>
                  <p className="text-[11px] text-slate-500 mt-0.5">Run a repo or app to create your workspace.</p>
                </div>
              </div>
            </div>

            {/* Local Apps */}
            <div className="flex flex-col gap-3">
              <div className="flex items-center justify-between px-1">
                <div className="flex items-center gap-2 text-[13px] font-bold text-slate-800">
                  <Folder size={16} className="text-amber-500" /> Local Apps
                </div>
                <a href="#" className="text-[11px] font-bold text-rose-500 hover:text-rose-600 transition-colors">Show all</a>
              </div>
              <div className="h-28 border border-rose-50 bg-[#FCFDFE] rounded-2xl flex items-center justify-center gap-4 p-4 shadow-[inset_0_0_20px_rgba(244,63,94,0.02)]">
                <div className="w-12 h-12 bg-white rounded-full flex items-center justify-center shadow-sm border border-slate-100 text-slate-300 shrink-0">
                  <Folder size={20} />
                </div>
                <div className="flex flex-col">
                  <h4 className="text-[13px] font-bold text-slate-700">No local apps found yet</h4>
                  <p className="text-[11px] text-slate-500 mt-0.5">Open a project folder to get started.</p>
                </div>
              </div>
            </div>
          </div>

          {/* Featured Apps Section */}
          <div>
            <div className="flex items-center justify-between px-1 mb-4">
              <div className="flex items-center gap-2 text-[14px] font-bold text-slate-800">
                <Sparkles size={16} className="text-slate-400" /> Featured Apps
              </div>
              <a href="#" className="text-[11px] font-bold text-rose-500 flex items-center gap-1 hover:text-rose-600 transition-colors">
                View all in store <ArrowRight size={12} strokeWidth={2.5} />
              </a>
            </div>
            
            <div className="grid grid-cols-3 gap-4">
              {/* AFFINE */}
              <div className="p-4 rounded-2xl border border-slate-100 bg-white shadow-sm flex gap-3 hover:shadow-md transition-shadow cursor-pointer">
                <div className="w-[72px] h-[72px] rounded-2xl bg-gradient-to-br from-rose-400 to-rose-600 flex items-center justify-center shrink-0 shadow-inner relative overflow-hidden">
                  {/* AFFINE SVG mock */}
                  <svg viewBox="0 0 24 24" fill="none" stroke="white" strokeWidth="2" strokeLinejoin="round" className="w-8 h-8 opacity-90">
                    <path d="M12 2L2 20h20L12 2z" />
                    <path d="M12 10l-4 7h8l-4-7z" />
                    <circle cx="12" cy="5" r="1.5" fill="white" stroke="none" />
                    <circle cx="5" cy="18" r="1.5" fill="white" stroke="none" />
                    <circle cx="19" cy="18" r="1.5" fill="white" stroke="none" />
                  </svg>
                </div>
                <div className="flex flex-col flex-1 h-full py-0.5">
                  <div className="flex items-center gap-2 mb-0.5">
                    <h4 className="font-bold text-[#0F172A] text-[13px]">AFFINE</h4>
                    <span className="text-[9px] font-bold text-rose-500 bg-rose-50 px-1.5 py-0.5 rounded-md">Store</span>
                  </div>
                  <p className="text-[10px] text-slate-500 leading-[1.35] mb-auto">The open-source workspace for note-taking and knowledge.</p>
                  <div className="flex items-center justify-between mt-2">
                    <div className="flex items-center gap-1 text-[10px] text-slate-500 font-medium">
                      <Star size={10} className="fill-amber-400 text-amber-400" />
                      <span className="text-slate-700">4.7</span>
                      <span className="opacity-40 px-0.5">•</span>
                      <span className="flex items-center gap-0.5"><Users size={9}/> 2.1k</span>
                    </div>
                    <button className="flex items-center gap-1 bg-gradient-to-r from-[#FF905A] to-[#F43F5E] text-white px-2.5 py-1.5 rounded-lg text-[10px] font-bold shadow-sm hover:opacity-90">
                      <Play size={8} className="fill-current" /> Launch
                    </button>
                  </div>
                </div>
              </div>

              {/* Open WebUI */}
              <div className="p-4 rounded-2xl border border-slate-100 bg-white shadow-sm flex gap-3 hover:shadow-md transition-shadow cursor-pointer">
                <div className="w-[72px] h-[72px] rounded-2xl bg-[#0F172A] flex items-center justify-center shrink-0 shadow-inner">
                  <div className="text-white font-bold text-[28px] tracking-tighter leading-none">OI</div>
                </div>
                <div className="flex flex-col flex-1 h-full py-0.5">
                  <div className="flex items-center gap-2 mb-0.5">
                    <h4 className="font-bold text-[#0F172A] text-[13px]">Open WebUI</h4>
                    <span className="text-[9px] font-bold text-rose-500 bg-rose-50 px-1.5 py-0.5 rounded-md">Store</span>
                  </div>
                  <p className="text-[10px] text-slate-500 leading-[1.35] mb-auto">Self-hosted AI interface for LLMs that just works.</p>
                  <div className="flex items-center justify-between mt-2">
                    <div className="flex items-center gap-1 text-[10px] text-slate-500 font-medium">
                      <Star size={10} className="fill-amber-400 text-amber-400" />
                      <span className="text-slate-700">4.8</span>
                      <span className="opacity-40 px-0.5">•</span>
                      <span className="flex items-center gap-0.5"><Users size={9}/> 3.3k</span>
                    </div>
                    <button className="flex items-center gap-1 bg-gradient-to-r from-[#FF905A] to-[#F43F5E] text-white px-2.5 py-1.5 rounded-lg text-[10px] font-bold shadow-sm hover:opacity-90">
                      <Play size={8} className="fill-current" /> Launch
                    </button>
                  </div>
                </div>
              </div>

              {/* Excalidraw */}
              <div className="p-4 rounded-2xl border border-slate-100 bg-white shadow-sm flex gap-3 hover:shadow-md transition-shadow cursor-pointer">
                <div className="w-[72px] h-[72px] rounded-2xl bg-gradient-to-br from-pink-400 to-rose-500 flex items-center justify-center shrink-0 shadow-inner">
                  <svg viewBox="0 0 24 24" fill="none" stroke="white" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" className="w-8 h-8 opacity-90">
                    <rect x="5" y="5" width="14" height="14" rx="2" transform="rotate(45 12 12)" />
                    <path d="M9 9l6 6M15 9l-6 6" />
                  </svg>
                </div>
                <div className="flex flex-col flex-1 h-full py-0.5">
                  <div className="flex items-center gap-2 mb-0.5">
                    <h4 className="font-bold text-[#0F172A] text-[13px]">Excalidraw</h4>
                    <span className="text-[9px] font-bold text-rose-500 bg-rose-50 px-1.5 py-0.5 rounded-md">Store</span>
                  </div>
                  <p className="text-[10px] text-slate-500 leading-[1.35] mb-auto">Virtual whiteboard for sketching diagrams and ideas.</p>
                  <div className="flex items-center justify-between mt-2">
                    <div className="flex items-center gap-1 text-[10px] text-slate-500 font-medium">
                      <Star size={10} className="fill-amber-400 text-amber-400" />
                      <span className="text-slate-700">4.6</span>
                      <span className="opacity-40 px-0.5">•</span>
                      <span className="flex items-center gap-0.5"><Users size={9}/> 1.6k</span>
                    </div>
                    <button className="flex items-center gap-1 bg-gradient-to-r from-[#FF905A] to-[#F43F5E] text-white px-2.5 py-1.5 rounded-lg text-[10px] font-bold shadow-sm hover:opacity-90">
                      <Play size={8} className="fill-current" /> Launch
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>

        {/* Settings Button */}
        <button className="absolute bottom-6 left-6 z-30 w-9 h-9 bg-white rounded-[10px] shadow-sm border border-slate-200 flex items-center justify-center text-slate-500 hover:text-slate-700 hover:bg-slate-50 hover:shadow transition-all">
          <Settings size={18} />
        </button>

      </div>

      <style dangerouslySetInnerHTML={{__html: `
        /* Orbit Styles */
        .orbit-container {
          position: absolute;
          top: 0;
          left: 0;
          width: 100%;
          height: 100%;
          overflow: hidden;
          transform-style: preserve-3d;
          z-index: 0;
          pointer-events: none;
          perspective: 1000px;
        }

        .orbit-wrapper {
          width: 540px;
          height: 540px;
          animation: orbitRotation 30s linear infinite;
          transform-style: preserve-3d;
          position: absolute;
          top: 30%; /* Shifted up slightly for better visual balance */
          left: 50%;
          margin-top: -270px;
          margin-left: -270px;
          transform-origin: center center;
        }

        .orbit-ring {
          position: absolute;
          width: 100%;
          height: 100%;
          border-radius: 50%;
          border-width: 1px;
          border-style: solid;
          transform-style: preserve-3d;
          transform-origin: center center;
          /* Color transition animation */
          animation: orbitColorLoop 15s linear infinite;
        }

        .orbit-ring-0 { transform: rotateY(0deg) translateZ(120px); }
        .orbit-ring-1 { transform: rotateY(60deg) translateZ(96px); }
        .orbit-ring-2 { transform: rotateY(120deg) translateZ(72px); }
        .orbit-ring-3 { transform: rotateY(180deg) translateZ(48px); }
        .orbit-ring-4 { transform: rotateY(240deg) translateZ(24px); }
        .orbit-ring-5 { transform: rotateY(300deg) translateZ(0px); }

        @keyframes orbitRotation {
          from { transform: rotateY(0deg) rotateX(30deg); }
          to { transform: rotateY(360deg) rotateX(30deg); }
        }

        @keyframes orbitColorLoop {
          0%   { border-color: rgba(244, 63, 94, 0.25); }   /* Rose */
          25%  { border-color: rgba(167, 139, 250, 0.25); } /* Violet */
          50%  { border-color: rgba(56, 189, 248, 0.25); }  /* Sky */
          75%  { border-color: rgba(251, 191, 36, 0.25); }  /* Amber */
          100% { border-color: rgba(244, 63, 94, 0.25); }   /* Rose */
        }

        /* Custom Scrollbar */
        .custom-scrollbar::-webkit-scrollbar {
          width: 6px;
        }
        .custom-scrollbar::-webkit-scrollbar-track {
          background: transparent;
        }
        .custom-scrollbar::-webkit-scrollbar-thumb {
          background-color: #E2E8F0;
          border-radius: 20px;
        }
        .custom-scrollbar::-webkit-scrollbar-thumb:hover {
          background-color: #CBD5E1;
        }
      `}} />
    </div>
  );
}

// ==========================================
// Component: Background Decorations (Orbit + Stars)
// ==========================================
const BackgroundDecorations = () => (
  <div className="absolute inset-0 overflow-hidden pointer-events-none z-0">
     {/* Dynamic Orbit Background */}
     <div className="orbit-container">
      <div className="orbit-wrapper">
        {[0, 60, 120, 180, 240, 300].map((deg) => (
          <div key={deg} className={`orbit-ring orbit-ring-${deg / 60}`} />
        ))}
      </div>
    </div>

     {/* Twinkling Stars */}
     <Star size={16} className="absolute top-20 left-[20%] text-amber-300 fill-current opacity-80 animate-[pulse_3s_ease-in-out_infinite]" />
     <Star size={24} className="absolute top-16 right-[25%] text-rose-300 fill-current opacity-60 animate-[pulse_4s_ease-in-out_infinite]" />
     <Star size={12} className="absolute top-40 right-[15%] text-purple-300 fill-current opacity-70 animate-[pulse_2.5s_ease-in-out_infinite]" />
     <Star size={14} className="absolute top-[30%] left-[10%] text-blue-300 fill-current opacity-60 animate-[pulse_3.5s_ease-in-out_infinite]" />
     
     {/* Four-point stars SVG */}
     <svg className="absolute top-[25%] right-[35%] w-4 h-4 text-emerald-300 opacity-60 animate-[pulse_3s_ease-in-out_infinite]" viewBox="0 0 24 24" fill="currentColor">
       <path d="M12 0L14 10L24 12L14 14L12 24L10 14L0 12L10 10Z" />
     </svg>
     <svg className="absolute top-[15%] left-[35%] w-3 h-3 text-rose-400 opacity-50 animate-[pulse_2s_ease-in-out_infinite]" viewBox="0 0 24 24" fill="currentColor">
       <path d="M12 0L14 10L24 12L14 14L12 24L10 14L0 12L10 10Z" />
     </svg>
  </div>
);