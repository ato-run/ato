import { Apple, Monitor, Terminal } from "lucide-react";

import { getPlatformBadgeMeta } from "../utils/platform";

interface PlatformBadgeProps {
  osArch: string;
  active: boolean;
}

function PlatformIcon({ osArch }: { osArch: string }): JSX.Element {
  const common = { size: 12, strokeWidth: 1.7, className: "platform-badge-icon" };
  const meta = getPlatformBadgeMeta(osArch);
  if (meta.family === "apple") {
    return <Apple {...common} />;
  }
  if (meta.family === "windows") {
    return <Monitor {...common} />;
  }
  return <Terminal {...common} />;
}

export function PlatformBadge({ osArch, active }: PlatformBadgeProps): JSX.Element {
  const meta = getPlatformBadgeMeta(osArch);
  return (
    <span className={`badge platform-badge ${active ? "badge-accent" : "badge-muted"}`}>
      <PlatformIcon osArch={osArch} />
      <span className="platform-badge-copy">
        <span>{meta.label}</span>
        {meta.arch ? <span className="platform-badge-arch">{meta.arch}</span> : null}
      </span>
    </span>
  );
}
