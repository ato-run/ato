import { cn } from "./utils";

const ICON_GRADIENTS = [
  "from-rose-400 to-rose-500",
  "from-blue-400 to-blue-500",
  "from-amber-400 to-orange-400",
  "from-emerald-400 to-emerald-500",
  "from-indigo-400 to-indigo-500",
  "from-slate-400 to-slate-500",
  "from-orange-400 to-orange-500",
  "from-pink-400 to-rose-500",
];

/**
 * Grid card variant — used in ato-dock grid view.
 */
export function AppGridCard({ capsule, index, onSelect, children }) {
  const gradient = ICON_GRADIENTS[index % ICON_GRADIENTS.length];
  const iconContent = capsule.iconText || (capsule.name ? capsule.name[0].toUpperCase() : "A");

  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "group p-5 rounded-2xl border border-slate-200 bg-white shadow-sm",
        "flex gap-4 text-left hover:shadow-md hover:border-slate-300",
        "transition-all duration-200",
      )}
      style={{ animationDelay: `${(index || 0) * 40}ms` }}
    >
      <div
        className={cn(
          "w-[72px] h-[72px] rounded-2xl bg-gradient-to-br",
          gradient,
          "flex items-center justify-center shrink-0 shadow-inner",
          "text-3xl font-bold text-white",
        )}
      >
        {iconContent}
      </div>
      <div className="flex flex-col flex-1 min-w-0 py-0.5">
        <div className="flex items-center gap-2 mb-1">
          <h4 className="font-bold text-[#0F172A] text-[14px] truncate">
            {capsule.name}
          </h4>
          {capsule.badge && (
            <span className={cn(
              "text-[9px] font-bold px-1.5 py-0.5 rounded-md shrink-0",
              capsule.badgeColor === "green"
                ? "text-emerald-600 bg-emerald-50"
                : capsule.badgeColor === "amber"
                  ? "text-amber-600 bg-amber-50"
                  : "text-rose-500 bg-rose-50",
            )}>
              {capsule.badge}
            </span>
          )}
        </div>
        <p className="text-[11px] text-slate-500 leading-[1.4] line-clamp-2 mb-auto">
          {capsule.tagline || capsule.description || ""}
        </p>
        {children && <div className="mt-auto">{children}</div>}
      </div>
    </button>
  );
}

/**
 * Compact card variant — used in sidebar list.
 */
export function AppCompactCard({ capsule, index, selected, onSelect }) {
  const gradient = ICON_GRADIENTS[index % ICON_GRADIENTS.length];
  const iconContent = capsule.iconText || (capsule.name ? capsule.name[0].toUpperCase() : "A");

  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "group relative w-full flex items-center gap-3 px-3 py-2.5 text-left transition-all duration-150 border-l-[3px]",
        selected
          ? "border-l-rose-500 bg-rose-50"
          : "border-l-transparent hover:bg-slate-50",
      )}
    >
      <div
        className={cn(
          "grid h-9 w-9 shrink-0 place-items-center rounded-lg",
          "bg-gradient-to-br text-sm font-bold text-white",
          gradient,
        )}
      >
        {iconContent}
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center justify-between gap-2">
          <span className="text-[13px] font-semibold text-slate-900 truncate">
            {capsule.name}
          </span>
          {capsule.statusDot && (
            <span className={cn(
              "h-[6px] w-[6px] shrink-0 rounded-full",
              capsule.statusDot,
            )} />
          )}
        </div>
        <div className="mt-0.5 text-[11px] text-slate-500 truncate">
          {capsule.subtitle || capsule.sourceUrl || ""}
        </div>
      </div>
    </button>
  );
}

/**
 * Featured card variant — horizontal with meta, used in ato-start featured.
 */
export function AppFeaturedCard({ app, gradient, onLaunch, onDetail }) {
  return (
    <div
      className={cn(
        "p-4 rounded-2xl border border-slate-200 bg-white shadow-sm",
        "flex gap-3 hover:shadow-md hover:border-slate-300",
        "transition-shadow cursor-pointer featured-card",
      )}
      onClick={onLaunch}
    >
      <div
        className={cn(
          "w-[72px] h-[72px] rounded-2xl bg-gradient-to-br",
          gradient || "from-rose-400 to-rose-600",
          "flex items-center justify-center shrink-0 shadow-inner",
          "text-3xl",
        )}
      >
        {app.icon || "⚡"}
      </div>
      <div className="flex flex-col flex-1 h-full py-0.5">
        <div className="flex items-center gap-2 mb-0.5">
          <h4 className="font-bold text-[#0F172A] text-[13px]">{app.label}</h4>
          {app.badge && (
            <span className="text-[9px] font-bold text-rose-500 bg-rose-50 px-1.5 py-0.5 rounded-md">
              {app.badge}
            </span>
          )}
        </div>
        <p className="text-[10px] text-slate-500 leading-[1.35] mb-auto">
          {app.description}
        </p>
        <div className="flex items-center justify-between mt-2">
          <div className="flex items-center gap-1 text-[10px] text-slate-500 font-medium">
            {app.rating != null && (
              <>
                <svg width="10" height="10" viewBox="0 0 24 24" fill="#FBBF24" stroke="#FBBF24" strokeWidth="1" className="shrink-0">
                  <path d="M12 2l3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01L12 2z" />
                </svg>
                <span className="text-slate-700">{app.rating}</span>
              </>
            )}
            {app.meta && (
              <>
                <span className="opacity-40 px-0.5">&bull;</span>
                <span>{app.meta}</span>
              </>
            )}
          </div>
          <button
            onClick={(e) => { e.stopPropagation(); onLaunch && onLaunch(); }}
            className="flex items-center gap-1 bg-gradient-to-r from-[#FF905A] to-[#F43F5E] text-white px-2.5 py-1.5 rounded-lg text-[10px] font-bold shadow-sm hover:opacity-90"
          >
            <svg width="8" height="8" viewBox="0 0 24 24" fill="currentColor">
              <polygon points="5 3 19 12 5 21 5 3" />
            </svg>
            {app.actionLabel || "Launch"}
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * Search bar component.
 */
export function SearchBar({ value, onChange, onSubmit, placeholder, hint = "\u2318K" }) {
  const handleKeyDown = (e) => {
    if (e.key === "Enter" && onSubmit) onSubmit(e.target.value);
  };

  return (
    <div className="w-full max-w-[640px] flex items-center gap-2 px-4 py-3 bg-white border border-rose-200 rounded-full shadow-[0_4px_20px_rgba(244,63,94,0.06)] transition-shadow focus-within:shadow-[0_4px_25px_rgba(244,63,94,0.12)] focus-within:border-rose-300">
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#94A3B8" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <circle cx="11" cy="11" r="8" />
        <path d="M21 21l-4.35-4.35" />
      </svg>
      <input
        type="text"
        value={value}
        onChange={(e) => onChange && onChange(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder={placeholder || "GitHub repo, local path, capsule, URL, or command"}
        className="flex-1 outline-none text-[13px] text-slate-700 placeholder:text-slate-400 font-medium bg-transparent"
        autoComplete="off"
        spellCheck="false"
      />
      {hint && (
        <kbd className="text-[10px] font-sans font-medium text-slate-400 bg-slate-50 border border-slate-200 px-1.5 py-0.5 rounded">
          {hint}
        </kbd>
      )}
    </div>
  );
}

/**
 * Empty state placeholder.
 */
export function EmptyState({ icon, title, description, actions }) {
  return (
    <div className="flex flex-col items-center py-20 text-center">
      {icon || (
        <div className="mx-auto grid h-14 w-14 place-items-center rounded-xl bg-slate-100">
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#94A3B8" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
            <path d="M21 16V8a2 2 0 00-1-1.73l-7-4a2 2 0 00-2 0l-7 4A2 2 0 002 8v8a2 2 0 001 1.73l7 4a2 2 0 002 0l7-4A2 2 0 0021 16z" />
          </svg>
        </div>
      )}
      <p className="mt-4 text-base font-semibold text-slate-600">{title}</p>
      {description && (
        <p className="mt-1 text-sm text-slate-400">{description}</p>
      )}
      {actions && <div className="mt-5 flex gap-3">{actions}</div>}
    </div>
  );
}
