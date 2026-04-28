const demoSections = [
  {
    title: "Launcher",
    detail: "Future route: capsule-host://launcher",
  },
  {
    title: "Settings",
    detail: "Future route: capsule-host://settings",
  },
  {
    title: "Capsule Detail",
    detail: "Future route: capsule-host://capsule/<pane-id>",
  },
];

export default function App() {
  return (
    <main className="app-shell">
      <section className="hero-card">
        <p className="eyebrow">Ato Desktop</p>
        <h1>Host Panel Frontend Skeleton</h1>
        <p className="lede">
          This frontend is the Phase 1 scaffold for host-owned panels rendered
          through Wry + React. Routing, bridge injection, and token codegen land
          in later PRs.
        </p>
      </section>

      <section className="info-grid" aria-label="Planned routes">
        {demoSections.map((section) => (
          <article className="info-card" key={section.title}>
            <h2>{section.title}</h2>
            <p>{section.detail}</p>
          </article>
        ))}
      </section>
    </main>
  );
}