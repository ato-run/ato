# wysiwyg-markdown-editor

`samples/wysiwyg-markdown-editor` is a Playground-ready, zero-build web capsule that provides a WYSIWYG Markdown editor.

## Features

- WYSIWYG editing with `contenteditable`
- Toolbar: H1, H2, Bold, Italic, Link, UL, OL, Code, Quote, Undo, Redo
- Live markdown generation and rendered preview
- Local persistence via `localStorage` key:
  - `ato.sample.wysiwyg-markdown-editor.v1`
- Keyboard shortcuts:
  - `Cmd/Ctrl + B` bold
  - `Cmd/Ctrl + I` italic
  - `Cmd/Ctrl + K` link
  - `Cmd/Ctrl + S` save

## Run locally

```bash
ato open /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/samples/wysiwyg-markdown-editor
```

## Expected Playground behavior

- Editor content updates markdown source and preview immediately
- Changes are auto-saved with debounce (300ms)
- Reload restores previous content from localStorage

## Constraints

- Zero-build static implementation (`index.html`, `styles.css`, `app.js`)
- No external CDN or third-party runtime dependencies
- Markdown conversion supports basic syntax only
