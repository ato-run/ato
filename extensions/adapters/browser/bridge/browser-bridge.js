(() => {
  "use strict";

  const bootstrap = globalThis.__ATO_BROWSER_BOOTSTRAP__;
  if (!Reflect.deleteProperty(globalThis, "__ATO_BROWSER_BOOTSTRAP__")) {
    globalThis.__ATO_BROWSER_BOOTSTRAP__ = undefined;
  }
  if (
    globalThis.top !== globalThis ||
    !isObject(bootstrap) ||
    bootstrap.protocol !== "ato.browser@1" ||
    bootstrap.expected_origin !== globalThis.location.origin ||
    typeof bootstrap.control_url !== "string" ||
    typeof bootstrap.channel_credential !== "string" ||
    typeof bootstrap.browser_session !== "string" ||
    !Array.isArray(bootstrap.allowed_non_text_codes) ||
    !["observe_and_apply", "apply_only"].includes(bootstrap.input_mode)
  ) {
    return;
  }

  const defaultCodes = new Set([
    "ArrowDown", "ArrowLeft", "ArrowRight", "ArrowUp", "Backspace",
    "Delete", "End", "Enter", "Escape", "Home", "Insert", "PageDown",
    "PageUp", "Space", "Tab",
  ]);
  const allowedCodes = new Set([
    ...defaultCodes,
    ...bootstrap.allowed_non_text_codes.filter((value) => typeof value === "string"),
  ]);
  const socket = new WebSocket(bootstrap.control_url);
  const pointerTargets = new Map();
  let acceptingInput = false;

  socket.addEventListener("open", () => {
    send({
      type: "hello",
      protocol: "ato.browser@1",
      channel_credential: bootstrap.channel_credential,
      browser_session: bootstrap.browser_session,
      top_level_origin: globalThis.location.origin,
    });
  });

  socket.addEventListener("message", (message) => {
    let value;
    try {
      value = JSON.parse(String(message.data));
    } catch {
      socket.close(1008, "invalid message");
      return;
    }
    if (!isObject(value)) return;
    if (
      value.type === "hello_ack" &&
      value.protocol === "ato.browser@1" &&
      value.browser_session === bootstrap.browser_session
    ) {
      acceptingInput = true;
      return;
    }
    if (value.type === "apply" && typeof value.request_id === "string") {
      try {
        dispatch(value.event);
        send({ type: "ack", request_id: value.request_id });
      } catch (error) {
        send({
          type: "error",
          request_id: value.request_id,
          reason: error instanceof Error ? error.message : "apply failed",
        });
      }
      return;
    }
    if (value.type === "quiesce" && typeof value.request_id === "string") {
      acceptingInput = false;
      send({ type: "quiesced", request_id: value.request_id });
    }
  });

  if (bootstrap.input_mode === "observe_and_apply") {
    globalThis.addEventListener("keydown", captureKeyboard, true);
    globalThis.addEventListener("keyup", captureKeyboard, true);
    globalThis.addEventListener("pointerdown", capturePointer, true);
    globalThis.addEventListener("pointerup", capturePointer, true);
    globalThis.addEventListener("pointercancel", capturePointer, true);
    globalThis.addEventListener("pointermove", capturePointer, true);
    globalThis.addEventListener("click", captureClick, true);
    globalThis.addEventListener("scroll", captureScroll, true);
  }

  function captureKeyboard(event) {
    if (!canCapture(event) || !allowedCodes.has(event.code)) return;
    send({
      type: "event",
      event: {
        type: "keyboard",
        kind: event.type === "keydown" ? "key_down" : "key_up",
        code: event.code,
        modifiers: modifiers(event),
      },
    });
  }

  function capturePointer(event) {
    if (!canCapture(event) || !["mouse", "pen"].includes(event.pointerType)) return;
    send({
      type: "event",
      event: {
        type: "pointer",
        kind: event.type.replace("pointer", "pointer_"),
        pointer_id: event.pointerId,
        pointer_type: event.pointerType,
        x_normalized: normalize(event.clientX, globalThis.innerWidth),
        y_normalized: normalize(event.clientY, globalThis.innerHeight),
        button: event.button,
        buttons: event.buttons,
      },
    });
  }

  function captureClick(event) {
    if (!canCapture(event)) return;
    send({
      type: "event",
      event: {
        type: "click",
        x_normalized: normalize(event.clientX, globalThis.innerWidth),
        y_normalized: normalize(event.clientY, globalThis.innerHeight),
        button: event.button,
      },
    });
  }

  function captureScroll(event) {
    if (!canCapture(event)) return;
    send({ type: "event", event: { type: "scroll", x: globalThis.scrollX, y: globalThis.scrollY } });
  }

  function canCapture(event) {
    return acceptingInput && event.isTrusted === true && socket.readyState === WebSocket.OPEN;
  }

  function dispatch(event) {
    if (!isObject(event) || typeof event.type !== "string") throw new Error("invalid Browser event");
    if (event.type === "keyboard") {
      const target = document.activeElement || document.body;
      target.dispatchEvent(new KeyboardEvent(
        event.kind === "key_down" ? "keydown" : "keyup",
        { code: event.code, ...modifierInit(event.modifiers), bubbles: true, cancelable: true },
      ));
      return;
    }
    if (event.type === "pointer") {
      const x = denormalize(event.x_normalized, globalThis.innerWidth);
      const y = denormalize(event.y_normalized, globalThis.innerHeight);
      let target = pointerTargets.get(event.pointer_id) || document.elementFromPoint(x, y) || document.body;
      if (event.kind === "pointer_down") pointerTargets.set(event.pointer_id, target);
      target.dispatchEvent(new PointerEvent(event.kind.replace("pointer_", "pointer"), {
        pointerId: event.pointer_id,
        pointerType: event.pointer_type,
        clientX: x,
        clientY: y,
        button: event.button,
        buttons: event.buttons,
        bubbles: true,
        cancelable: true,
      }));
      if (event.kind === "pointer_up" || event.kind === "pointer_cancel") pointerTargets.delete(event.pointer_id);
      return;
    }
    if (event.type === "click") {
      const x = denormalize(event.x_normalized, globalThis.innerWidth);
      const y = denormalize(event.y_normalized, globalThis.innerHeight);
      const target = document.elementFromPoint(x, y) || document.body;
      target.dispatchEvent(new MouseEvent("click", {
        clientX: x,
        clientY: y,
        button: event.button,
        bubbles: true,
        cancelable: true,
      }));
      return;
    }
    if (event.type === "scroll") {
      globalThis.scrollTo(event.x, event.y);
      return;
    }
    throw new Error("unsupported Browser event");
  }

  function send(value) {
    if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(value));
  }

  function modifiers(event) {
    return { alt: event.altKey, control: event.ctrlKey, meta: event.metaKey, shift: event.shiftKey };
  }

  function modifierInit(value) {
    if (!isObject(value)) throw new Error("invalid modifiers");
    return { altKey: value.alt, ctrlKey: value.control, metaKey: value.meta, shiftKey: value.shift };
  }

  function normalize(value, extent) {
    if (!Number.isFinite(value) || !Number.isFinite(extent) || extent <= 0) return 0;
    return Math.min(1, Math.max(0, value / extent));
  }

  function denormalize(value, extent) {
    if (!Number.isFinite(value) || value < 0 || value > 1) throw new Error("invalid normalized coordinate");
    return value * extent;
  }

  function isObject(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value);
  }
})();
