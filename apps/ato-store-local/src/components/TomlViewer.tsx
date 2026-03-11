interface TomlViewerProps {
  rawToml: string;
}

function renderTomlLine(line: string): JSX.Element {
  if (line.startsWith("#")) {
    return <span className="toml-comment">{line}</span>;
  }
  if (line.startsWith("[") && line.endsWith("]")) {
    return <span className="toml-section">{line}</span>;
  }
  const eqIndex = line.indexOf("=");
  if (eqIndex > 0) {
    const key = line.slice(0, eqIndex).trimEnd();
    const value = line.slice(eqIndex + 1).trimStart();
    return (
      <>
        <span className="toml-key">{key}</span>
        <span className="toml-sep"> = </span>
        <span className="toml-val">{value}</span>
      </>
    );
  }
  return <span className="toml-plain">{line}</span>;
}

export function TomlViewer({ rawToml }: TomlViewerProps): JSX.Element {
  const lines = rawToml.split("\n");
  return (
    <div className="toml-body">
      {lines.map((line, index) => {
        return (
          <div key={`${index}-${line}`} className="toml-line">
            <span className="toml-num">{index + 1}</span>
            <span className="toml-text">{renderTomlLine(line)}</span>
          </div>
        );
      })}
    </div>
  );
}
