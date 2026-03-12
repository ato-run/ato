export function LocalFooter(): JSX.Element {
  return (
    <footer className="border-t border-[var(--border,#e5e5e5)] bg-[var(--bg-card,#fff)] mt-0">
      <div className="max-w-6xl mx-auto px-6 py-6 flex flex-col md:flex-row justify-between items-center gap-4">
        <div className="flex items-center">
          <img
            src="/favicon.svg"
            alt=""
            aria-hidden="true"
            className="w-5 h-5 mr-2 object-contain"
            onError={(e) => { (e.currentTarget as HTMLImageElement).style.display = "none"; }}
          />
          <span
            className="text-sm text-[var(--fg-secondary,#525252)]"
            style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: "12px" }}
          >
            ato local dock
          </span>
        </div>
        <nav className="flex space-x-6 text-sm text-[var(--fg-secondary,#525252)]" aria-label="footer">
          <a
            href="https://store.ato.run/privacy"
            className="hover:text-[var(--fg,#0a0a0a)] transition-colors"
            target="_blank"
            rel="noreferrer"
          >
            Privacy
          </a>
        </nav>
      </div>
    </footer>
  );
}
