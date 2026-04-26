(function main() {
  const STORAGE_KEY = "ato.sample.wysiwyg-markdown-editor.v1";
  const SAVE_DEBOUNCE_MS = 300;
  const DEFAULT_HTML =
    "<h1>WYSIWYG Markdown Editor</h1><p>Type here, then check preview and markdown output.</p><ul><li>Bold / Italic / Link</li><li>Lists / Quote / Code</li></ul>";

  const editor = document.getElementById("editor");
  const preview = document.getElementById("preview-output");
  const markdownSource = document.getElementById("markdown-source");
  const saveState = document.getElementById("save-state");
  const toolbar = document.querySelector(".toolbar");
  const copyButton = document.getElementById("copy-markdown");

  let saveTimer = null;
  let currentState = "saved";

  function setState(next) {
    if (currentState === next) return;
    currentState = next;
    saveState.textContent = next === "saved" ? "Saved" : "Unsaved";
    saveState.classList.remove("saved", "unsaved");
    saveState.classList.add(next === "saved" ? "saved" : "unsaved");
  }

  function ensureFocus() {
    editor.focus();
  }

  function applyCommand(command) {
    ensureFocus();
    switch (command) {
      case "h1":
        document.execCommand("formatBlock", false, "<h1>");
        break;
      case "h2":
        document.execCommand("formatBlock", false, "<h2>");
        break;
      case "bold":
        document.execCommand("bold", false);
        break;
      case "italic":
        document.execCommand("italic", false);
        break;
      case "link": {
        const href = window.prompt("Enter URL", "https://");
        if (!href) return;
        document.execCommand("createLink", false, href.trim());
        break;
      }
      case "ul":
        document.execCommand("insertUnorderedList", false);
        break;
      case "ol":
        document.execCommand("insertOrderedList", false);
        break;
      case "code":
        insertCodeAtSelection();
        break;
      case "quote":
        document.execCommand("formatBlock", false, "<blockquote>");
        break;
      case "undo":
        document.execCommand("undo", false);
        break;
      case "redo":
        document.execCommand("redo", false);
        break;
      default:
        break;
    }
    onEditorChange();
  }

  function insertCodeAtSelection() {
    const selection = window.getSelection();
    if (!selection || selection.rangeCount === 0) return;
    const selectedText = selection.toString().trim();
    const payload = selectedText ? selectedText : "code";
    document.execCommand("insertHTML", false, "<code>" + escapeHtml(payload) + "</code>");
  }

  function loadInitialContent() {
    try {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (!raw) {
        editor.innerHTML = DEFAULT_HTML;
        return;
      }
      const parsed = JSON.parse(raw);
      if (parsed && typeof parsed.html === "string" && parsed.html.trim().length > 0) {
        editor.innerHTML = parsed.html;
      } else {
        editor.innerHTML = DEFAULT_HTML;
      }
    } catch (_error) {
      editor.innerHTML = DEFAULT_HTML;
    }
  }

  function saveNow() {
    const payload = { html: editor.innerHTML, updated_at: Date.now() };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(payload));
    setState("saved");
  }

  function scheduleSave() {
    if (saveTimer) {
      window.clearTimeout(saveTimer);
    }
    saveTimer = window.setTimeout(function onDebouncedSave() {
      saveNow();
    }, SAVE_DEBOUNCE_MS);
  }

  function onEditorChange() {
    setState("unsaved");
    syncViews();
    scheduleSave();
  }

  function syncViews() {
    const markdown = htmlToMarkdown(editor.innerHTML);
    markdownSource.value = markdown;
    preview.innerHTML = markdownToHtml(markdown);
  }

  function htmlToMarkdown(html) {
    const parser = new DOMParser();
    const doc = parser.parseFromString(html, "text/html");
    const chunks = [];
    const bodyNodes = Array.from(doc.body.childNodes);

    bodyNodes.forEach(function eachNode(node) {
      chunks.push(blockNodeToMarkdown(node));
    });

    return chunks.join("").replace(/\n{3,}/g, "\n\n").trim();
  }

  function blockNodeToMarkdown(node) {
    if (node.nodeType === Node.TEXT_NODE) {
      return normalizeText(node.textContent || "") + "\n\n";
    }
    if (node.nodeType !== Node.ELEMENT_NODE) {
      return "";
    }

    const tag = node.tagName.toLowerCase();
    switch (tag) {
      case "h1":
        return "# " + inlineNodesToMarkdown(node.childNodes).trim() + "\n\n";
      case "h2":
        return "## " + inlineNodesToMarkdown(node.childNodes).trim() + "\n\n";
      case "p":
      case "div":
        return inlineNodesToMarkdown(node.childNodes).trim() + "\n\n";
      case "ul":
        return listToMarkdown(node, false) + "\n\n";
      case "ol":
        return listToMarkdown(node, true) + "\n\n";
      case "pre":
        return "```\n" + (node.textContent || "").trim() + "\n```\n\n";
      case "blockquote":
        return (
          inlineNodesToMarkdown(node.childNodes)
            .split("\n")
            .map(function mapQuote(line) {
              return line.trim().length > 0 ? "> " + line : ">";
            })
            .join("\n") + "\n\n"
        );
      case "br":
        return "\n";
      default: {
        const fallback = inlineNodesToMarkdown(node.childNodes).trim();
        return fallback.length > 0 ? fallback + "\n\n" : "";
      }
    }
  }

  function listToMarkdown(listNode, ordered) {
    const items = Array.from(listNode.children).filter(function filterLi(node) {
      return node.tagName && node.tagName.toLowerCase() === "li";
    });
    return items
      .map(function mapLi(item, index) {
        const prefix = ordered ? String(index + 1) + ". " : "- ";
        return prefix + inlineNodesToMarkdown(item.childNodes).trim();
      })
      .join("\n");
  }

  function inlineNodesToMarkdown(nodeList) {
    return Array.from(nodeList)
      .map(function mapInline(node) {
        return inlineNodeToMarkdown(node);
      })
      .join("")
      .replace(/[ \t]+\n/g, "\n")
      .replace(/\n{3,}/g, "\n\n");
  }

  function inlineNodeToMarkdown(node) {
    if (node.nodeType === Node.TEXT_NODE) {
      return normalizeText(node.textContent || "");
    }
    if (node.nodeType !== Node.ELEMENT_NODE) {
      return "";
    }
    const tag = node.tagName.toLowerCase();
    const content = inlineNodesToMarkdown(node.childNodes).trim();

    switch (tag) {
      case "strong":
      case "b":
        return content.length > 0 ? "**" + content + "**" : "";
      case "em":
      case "i":
        return content.length > 0 ? "*" + content + "*" : "";
      case "a": {
        const href = node.getAttribute("href") || "";
        if (!href) return content;
        const label = content.length > 0 ? content : href;
        return "[" + label + "](" + href + ")";
      }
      case "code":
        return content.length > 0 ? "`" + content + "`" : "";
      case "br":
        return "\n";
      default:
        return content.length > 0 ? content : normalizeText(node.textContent || "");
    }
  }

  function markdownToHtml(markdown) {
    const lines = markdown.replace(/\r\n/g, "\n").split("\n");
    const html = [];
    let inCodeBlock = false;
    let codeLines = [];
    let paragraphLines = [];
    let listType = "";
    let listItems = [];

    function flushParagraph() {
      if (paragraphLines.length === 0) return;
      const text = paragraphLines.join(" ").trim();
      if (text.length > 0) {
        html.push("<p>" + formatInlineMarkdown(text) + "</p>");
      }
      paragraphLines = [];
    }

    function flushList() {
      if (!listType || listItems.length === 0) return;
      html.push("<" + listType + ">");
      listItems.forEach(function eachItem(item) {
        html.push("<li>" + formatInlineMarkdown(item) + "</li>");
      });
      html.push("</" + listType + ">");
      listType = "";
      listItems = [];
    }

    function flushCodeBlock() {
      if (!inCodeBlock) return;
      html.push("<pre><code>" + escapeHtml(codeLines.join("\n")) + "</code></pre>");
      inCodeBlock = false;
      codeLines = [];
    }

    lines.forEach(function eachLine(line) {
      if (line.trim().startsWith("```")) {
        flushParagraph();
        flushList();
        if (inCodeBlock) {
          flushCodeBlock();
        } else {
          inCodeBlock = true;
          codeLines = [];
        }
        return;
      }

      if (inCodeBlock) {
        codeLines.push(line);
        return;
      }

      if (line.trim().length === 0) {
        flushParagraph();
        flushList();
        return;
      }

      if (line.startsWith("# ")) {
        flushParagraph();
        flushList();
        html.push("<h1>" + formatInlineMarkdown(line.slice(2).trim()) + "</h1>");
        return;
      }

      if (line.startsWith("## ")) {
        flushParagraph();
        flushList();
        html.push("<h2>" + formatInlineMarkdown(line.slice(3).trim()) + "</h2>");
        return;
      }

      if (line.startsWith("> ")) {
        flushParagraph();
        flushList();
        html.push("<blockquote><p>" + formatInlineMarkdown(line.slice(2).trim()) + "</p></blockquote>");
        return;
      }

      if (/^-\s+/.test(line)) {
        flushParagraph();
        if (listType && listType !== "ul") flushList();
        listType = "ul";
        listItems.push(line.replace(/^-\s+/, "").trim());
        return;
      }

      if (/^\d+\.\s+/.test(line)) {
        flushParagraph();
        if (listType && listType !== "ol") flushList();
        listType = "ol";
        listItems.push(line.replace(/^\d+\.\s+/, "").trim());
        return;
      }

      if (listType) {
        flushList();
      }
      paragraphLines.push(line.trim());
    });

    flushParagraph();
    flushList();
    flushCodeBlock();

    if (html.length === 0) {
      return "<p></p>";
    }
    return html.join("");
  }

  function formatInlineMarkdown(text) {
    let output = escapeHtml(text);
    output = output.replace(/\[([^\]]+)\]\((https?:\/\/[^)]+)\)/g, '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>');
    output = output.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
    output = output.replace(/\*([^*]+)\*/g, "<em>$1</em>");
    output = output.replace(/`([^`]+)`/g, "<code>$1</code>");
    return output;
  }

  function normalizeText(value) {
    return value.replace(/\u00a0/g, " ").replace(/\s+/g, " ");
  }

  function escapeHtml(value) {
    return String(value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function handleKeyboardShortcut(event) {
    const isMac = navigator.platform.toUpperCase().indexOf("MAC") >= 0;
    const withModifier = isMac ? event.metaKey : event.ctrlKey;
    if (!withModifier) return;

    const key = event.key.toLowerCase();
    if (key === "b") {
      event.preventDefault();
      applyCommand("bold");
      return;
    }
    if (key === "i") {
      event.preventDefault();
      applyCommand("italic");
      return;
    }
    if (key === "k") {
      event.preventDefault();
      applyCommand("link");
      return;
    }
    if (key === "s") {
      event.preventDefault();
      saveNow();
    }
  }

  function copyMarkdown() {
    const value = markdownSource.value;
    if (!value) return;
    if (navigator.clipboard && typeof navigator.clipboard.writeText === "function") {
      navigator.clipboard.writeText(value).then(
        function onCopied() {
          copyButton.textContent = "Copied";
          window.setTimeout(function resetCopyLabel() {
            copyButton.textContent = "Copy";
          }, 1000);
        },
        function onCopyError() {
          fallbackCopy();
        },
      );
      return;
    }
    fallbackCopy();
  }

  function fallbackCopy() {
    markdownSource.focus();
    markdownSource.select();
    document.execCommand("copy");
    editor.focus();
  }

  toolbar.addEventListener("click", function onToolbarClick(event) {
    const button = event.target.closest("button[data-command]");
    if (!button) return;
    const command = button.getAttribute("data-command");
    if (!command) return;
    applyCommand(command);
  });

  editor.addEventListener("input", onEditorChange);
  editor.addEventListener("keydown", handleKeyboardShortcut);
  copyButton.addEventListener("click", copyMarkdown);

  loadInitialContent();
  syncViews();
  setState("saved");
})();
