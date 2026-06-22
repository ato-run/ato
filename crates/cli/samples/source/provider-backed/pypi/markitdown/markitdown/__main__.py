from __future__ import annotations

import argparse
from pathlib import Path


class MissingDependencyException(RuntimeError):
    pass


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("input_path")
    parser.add_argument("-o", "--output", required=True)
    args = parser.parse_args()

    input_path = Path(args.input_path)
    output_path = Path(args.output)

    if input_path.suffix.lower() == ".pdf":
        try:
            import markitdown_pdf_helper  # noqa: F401
        except ImportError as exc:
            raise MissingDependencyException(
                "PdfConverter recognized the input as a potential .pdf file, but the dependencies needed to read .pdf files have not been installed. To resolve this error, include the optional dependency [pdf] or [all] when installing MarkItDown."
            ) from exc

    output_path.write_text("converted\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())