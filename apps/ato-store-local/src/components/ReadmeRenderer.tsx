interface ReadmeRendererProps {
  readme: string;
}

export function ReadmeRenderer({ readme }: ReadmeRendererProps): JSX.Element {
  const lines = readme.split("\n");
  const blocks: JSX.Element[] = [];
  let inCode = false;
  let codeLines: string[] = [];
  let listBuffer: string[] = [];

  const flushList = (): void => {
    if (listBuffer.length > 0) {
      blocks.push(
        <ul key={`list-${blocks.length}`}>
          {listBuffer.map((line) => (
            <li key={`${line}-${blocks.length}`}>{line}</li>
          ))}
        </ul>,
      );
      listBuffer = [];
    }
  };

  lines.forEach((raw, index) => {
    const line = raw.trimEnd();
    if (line.startsWith("```") && !inCode) {
      flushList();
      inCode = true;
      codeLines = [];
      return;
    }
    if (line.startsWith("```") && inCode) {
      blocks.push(
        <pre key={`code-${index}`} className="readme-code">
          {codeLines.join("\n")}
        </pre>,
      );
      inCode = false;
      codeLines = [];
      return;
    }
    if (inCode) {
      codeLines.push(line);
      return;
    }
    if (line.startsWith("# ")) {
      flushList();
      blocks.push(<h1 key={`h1-${index}`}>{line.slice(2)}</h1>);
      return;
    }
    if (line.startsWith("## ")) {
      flushList();
      blocks.push(<h2 key={`h2-${index}`}>{line.slice(3)}</h2>);
      return;
    }
    if (line.startsWith("- ")) {
      listBuffer.push(line.slice(2));
      return;
    }
    flushList();
    if (line.length > 0) {
      blocks.push(<p key={`p-${index}`}>{line}</p>);
    }
  });

  flushList();

  return <div className="readme-body">{blocks}</div>;
}
