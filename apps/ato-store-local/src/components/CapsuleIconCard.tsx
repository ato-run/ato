import { Box, Globe, Package, Zap } from "lucide-react";
import type { Capsule } from "../types";

interface CapsuleIconCardProps {
  capsule: Capsule;
  onClick: (capsule: Capsule) => void;
}

function IconForCapsule({ iconKey }: { iconKey: Capsule["iconKey"] }): JSX.Element {
  const common = { size: 24, strokeWidth: 1.5 };
  if (iconKey === "globe") {
    return <Globe {...common} />;
  }
  if (iconKey === "zap") {
    return <Zap {...common} />;
  }
  if (iconKey === "box") {
    return <Box {...common} />;
  }
  return <Package {...common} />;
}

export function CapsuleIconCard({ capsule, onClick }: CapsuleIconCardProps): JSX.Element {
  return (
    <button className="icon-card" type="button" onClick={() => onClick(capsule)}>
      <span className="icon-frame">
        {capsule.storeMetadata?.iconUrl ? (
          <img src={capsule.storeMetadata.iconUrl} alt={`${capsule.scopedId} icon`} className="icon-frame-img" />
        ) : (
          <IconForCapsule iconKey={capsule.iconKey} />
        )}
      </span>
      <span className="icon-title">{capsule.scopedId}</span>
    </button>
  );
}
