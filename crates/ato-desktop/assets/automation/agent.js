// Ato Desktop Automation Agent — injected by AutomationHost when pane has Automation capability.
// Implements a Playwright-compatible DOM API surface for AI agent use.
(function () {
  if (window.__atoAgent) return;

  // ── Stable element refs ─────────────────────────────────────────────────
  // WeakRef allows garbage-collection of removed elements; Map enables ref→el lookup.
  const _elToRef = new WeakMap(); // element → "e<n>"
  const _refToEl = new Map();     // "e<n>" → WeakRef<element>
  let _refCounter = 0;

  function assignRef(el) {
    if (_elToRef.has(el)) return _elToRef.get(el);
    const ref = 'e' + (++_refCounter);
    _elToRef.set(el, ref);
    _refToEl.set(ref, new WeakRef(el));
    return ref;
  }

  function findByRef(ref) {
    const wr = _refToEl.get(ref);
    if (!wr) return null;
    const el = wr.deref();
    if (!el || !document.contains(el)) {
      _refToEl.delete(ref);
      return null;
    }
    return el;
  }

  // ── Aria snapshot ────────────────────────────────────────────────────────
  const SKIP_TAGS = new Set(['script', 'style', 'head', 'meta', 'link', 'noscript', 'svg', 'path']);
  const SKIP_ROLES = new Set(['none', 'presentation']);

  const TAG_ROLES = {
    a: 'link', button: 'button', select: 'listbox', textarea: 'textbox',
    img: 'img', nav: 'navigation', main: 'main', header: 'banner',
    footer: 'contentinfo', aside: 'complementary', section: 'region',
    article: 'article', form: 'form', dialog: 'dialog',
    ul: 'list', ol: 'list', li: 'listitem',
    table: 'table', tr: 'row', th: 'columnheader', td: 'cell',
    h1: 'heading', h2: 'heading', h3: 'heading',
    h4: 'heading', h5: 'heading', h6: 'heading',
  };

  const INPUT_ROLES = {
    checkbox: 'checkbox', radio: 'radio', button: 'button',
    submit: 'button', reset: 'button', range: 'slider', number: 'spinbutton',
  };

  function roleOf(el) {
    const r = el.getAttribute('role');
    if (r) return r;
    const tag = el.tagName.toLowerCase();
    if (tag === 'input') return INPUT_ROLES[el.type] || 'textbox';
    return TAG_ROLES[tag] || 'generic';
  }

  function nameOf(el) {
    const ariaLabel = el.getAttribute('aria-label');
    if (ariaLabel) return ariaLabel.trim();
    const labelledBy = el.getAttribute('aria-labelledby');
    if (labelledBy) {
      const target = document.getElementById(labelledBy);
      if (target) return target.textContent.trim();
    }
    const tag = el.tagName.toLowerCase();
    if (tag === 'img') return el.getAttribute('alt') || '';
    if (tag === 'input') return el.getAttribute('placeholder') || el.getAttribute('title') || '';
    if (tag === 'a' || tag === 'button') return el.textContent.trim().slice(0, 80);
    return el.getAttribute('title') || '';
  }

  function isVisible(el) {
    const s = window.getComputedStyle(el);
    return s.display !== 'none' && s.visibility !== 'hidden' && parseFloat(s.opacity) > 0;
  }

  function buildNode(el, depth) {
    if (depth > 25) return null;
    const tag = el.tagName.toLowerCase();
    if (SKIP_TAGS.has(tag)) return null;
    if (!isVisible(el)) return null;
    const role = roleOf(el);
    if (SKIP_ROLES.has(role)) return null;

    const ref = assignRef(el);
    const name = nameOf(el);
    const node = { role, ref };
    if (name) node.name = name;

    if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {
      node.value = el.value;
    } else if (el.tagName === 'SELECT') {
      node.value = el.options[el.selectedIndex] ? el.options[el.selectedIndex].text : '';
    }
    if (el.type === 'checkbox' || el.type === 'radio') node.checked = el.checked;
    if (el.disabled || el.getAttribute('aria-disabled') === 'true') node.disabled = true;

    const children = [];
    for (const child of el.children) {
      const n = buildNode(child, depth + 1);
      if (n) children.push(n);
    }
    if (children.length) node.children = children;
    return node;
  }

  // ── Console capture ──────────────────────────────────────────────────────
  const _consoleBuf = [];
  const MAX_MSGS = 200;

  ['log', 'info', 'warn', 'error', 'debug'].forEach(function (level) {
    const orig = console[level].bind(console);
    console[level] = function () {
      orig.apply(console, arguments);
      if (_consoleBuf.length < MAX_MSGS) {
        _consoleBuf.push({
          level,
          text: Array.prototype.slice.call(arguments).map(function (a) {
            try { return typeof a === 'object' ? JSON.stringify(a) : String(a); } catch (_) { return String(a); }
          }).join(' '),
          timestamp: Date.now(),
        });
      }
    };
  });

  // ── Key codes ────────────────────────────────────────────────────────────
  const KEY_CODES = {
    Enter: 13, Escape: 27, ' ': 32, Space: 32,
    ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40,
    Backspace: 8, Tab: 9, Delete: 46,
  };

  function keyCode(key) {
    return KEY_CODES[key] !== undefined ? KEY_CODES[key] : (key.charCodeAt(0) || 0);
  }

  // ── Exported API ─────────────────────────────────────────────────────────
  window.__atoAgent = {
    snapshot: function () {
      try {
        const root = document.body || document.documentElement;
        const children = [];
        for (const child of root.children) {
          const n = buildNode(child, 0);
          if (n) children.push(n);
        }
        return JSON.stringify({ role: 'WebArea', name: document.title, children });
      } catch (e) {
        return JSON.stringify({ error: String(e) });
      }
    },

    isPresent: function (selector) {
      try {
        const el = document.querySelector(selector);
        return JSON.stringify({ found: !!el });
      } catch (e) {
        return JSON.stringify({ found: false, error: String(e) });
      }
    },

    click: function (ref) {
      const el = findByRef(ref);
      if (!el) return JSON.stringify({ error: 'element not found: ' + ref });
      el.click();
      if (el.focus) el.focus();
      return JSON.stringify({ ok: true });
    },

    fill: function (ref, value) {
      const el = findByRef(ref);
      if (!el) return JSON.stringify({ error: 'element not found: ' + ref });
      // Use native value setter to properly trigger React controlled components.
      const nativeSetter = Object.getOwnPropertyDescriptor(
        el.tagName === 'TEXTAREA' ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype,
        'value'
      );
      if (nativeSetter && nativeSetter.set) {
        nativeSetter.set.call(el, value);
      } else {
        el.value = value;
      }
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.dispatchEvent(new Event('change', { bubbles: true }));
      return JSON.stringify({ ok: true });
    },

    type: function (ref, text) {
      const el = findByRef(ref);
      if (!el) return JSON.stringify({ error: 'element not found: ' + ref });
      if (el.focus) el.focus();
      for (let i = 0; i < text.length; i++) {
        const char = text[i];
        const kc = keyCode(char);
        el.dispatchEvent(new KeyboardEvent('keydown', { key: char, keyCode: kc, bubbles: true }));
        el.dispatchEvent(new KeyboardEvent('keypress', { key: char, keyCode: kc, bubbles: true }));
        if ('value' in el) el.value += char;
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new KeyboardEvent('keyup', { key: char, keyCode: kc, bubbles: true }));
      }
      return JSON.stringify({ ok: true });
    },

    selectOption: function (ref, value) {
      const el = findByRef(ref);
      if (!el) return JSON.stringify({ error: 'element not found: ' + ref });
      for (let i = 0; i < (el.options || []).length; i++) {
        const opt = el.options[i];
        if (opt.value === value || opt.text === value) {
          el.value = opt.value;
          el.dispatchEvent(new Event('change', { bubbles: true }));
          return JSON.stringify({ ok: true });
        }
      }
      return JSON.stringify({ error: 'option not found: ' + value });
    },

    check: function (ref, checked) {
      const el = findByRef(ref);
      if (!el) return JSON.stringify({ error: 'element not found: ' + ref });
      el.checked = !!checked;
      el.dispatchEvent(new Event('change', { bubbles: true }));
      return JSON.stringify({ ok: true });
    },

    pressKey: function (key) {
      const target = document.activeElement || document.body;
      const kc = keyCode(key);
      target.dispatchEvent(new KeyboardEvent('keydown', { key, keyCode: kc, bubbles: true, cancelable: true }));
      target.dispatchEvent(new KeyboardEvent('keypress', { key, keyCode: kc, bubbles: true, cancelable: true }));
      target.dispatchEvent(new KeyboardEvent('keyup', { key, keyCode: kc, bubbles: true }));
      return JSON.stringify({ ok: true });
    },

    // waitFor does an immediate synchronous DOM check.
    // Rust-side polling is used for actual wait semantics (re-queued requests).
    waitFor: function (selector) {
      try {
        const el = document.querySelector(selector);
        return JSON.stringify({ found: !!el });
      } catch (e) {
        return JSON.stringify({ found: false, error: String(e) });
      }
    },

    evaluate: function (expression) {
      try {
        // eslint-disable-next-line no-eval
        const result = eval(expression);
        return JSON.stringify({ result: result !== undefined ? result : null });
      } catch (e) {
        return JSON.stringify({ error: String(e) });
      }
    },

    getConsoleMessages: function () {
      const msgs = _consoleBuf.splice(0);
      return JSON.stringify(msgs);
    },

    verifyTextVisible: function (text) {
      const found = !!(document.body && document.body.textContent.includes(text));
      return JSON.stringify({ visible: found });
    },

    verifyElementVisible: function (ref) {
      const el = findByRef(ref);
      if (!el) return JSON.stringify({ visible: false });
      const rect = el.getBoundingClientRect();
      const visible = rect.width > 0 && rect.height > 0 && document.contains(el);
      return JSON.stringify({ visible });
    },
  };
})();
