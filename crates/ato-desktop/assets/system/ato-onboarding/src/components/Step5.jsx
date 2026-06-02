import React from 'react'
import { ShieldCheck, Box, Cpu } from 'lucide-react'

// Runtime safety: two default-on, opt-out toggles. Both stay enabled unless
// the user explicitly unchecks them. State is owned by App so the keyboard
// "finish" path and the button submit the same values.
function ToggleCard({ checked, onToggle, icon: Icon, title, children }) {
  return (
    <button
      type="button"
      onClick={onToggle}
      className={`w-full text-left rounded-[20px] border p-4 flex gap-3 items-start transition-all ${
        checked
          ? 'bg-[#F5F3FF] border-[#DDD6FE]'
          : 'bg-white border-slate-200'
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
  hostDetectionEnabled,
  setHostDetectionEnabled,
}) {
  return (
    <div className="flex flex-col h-full p-8">
      <div className="shrink-0">
        <div className="text-[#8B5CF6] font-bold tracking-widest text-sm mb-4 mt-2">5 / 5</div>

        <div className="flex items-center gap-2 mb-2">
          <ShieldCheck className="text-[#8B5CF6]" size={28} strokeWidth={2} />
          <h1 className="text-[36px] leading-tight font-extrabold text-[#0F172A] tracking-tight">
            Runtime safety
          </h1>
        </div>

        <p className="text-[16px] text-slate-500 mb-6 pr-4 leading-relaxed">
          Ato can check your local machine before launching recipes.
          These are on by default — you can change them later in Settings.
        </p>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto flex flex-col gap-3">
        <ToggleCard
          checked={podmanEnabled}
          onToggle={() => setPodmanEnabled((v) => !v)}
          icon={Box}
          title="Podman をコンテナ実行に使用する"
        >
          OCI recipe が Podman を必要とする場合に利用します。後から Settings で無効化できます。
        </ToggleCard>

        <ToggleCard
          checked={hostDetectionEnabled}
          onToggle={() => setHostDetectionEnabled((v) => !v)}
          icon={Cpu}
          title="ホストデバイス検出を許可する"
        >
          GPU など追加のホストデバイスをローカルで検出し、起動前の互換性チェックに使います。
          OS / architecture など実行に必須の情報は、この設定に関わらず常に検出します。
          検出結果は明示的な同意なしに外部送信しません。
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
