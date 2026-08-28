const BROWSER_RUNNER_BRIDGE_VERSION = "0.1.0";
const BROWSER_RUNNER_DATA_CHANNEL_LABEL = "ato.browser.control.v1";
const BROWSER_RUNNER_INPUT_PROFILE = "browser_dom";
const ID$1 = /^[A-Za-z0-9_-]{8,160}$/;
const MAX_MESSAGE_BYTES = 64 * 1024;
const MAX_DEDUPE_ENTRIES = 2048;
class BrowserRunnerBridge {
  constructor(options) {
    this.options = options;
    validateIdentity(options);
    if (normalizeOrigin(options.expected_origin) !== options.expected_origin) {
      throw new TypeError("expected_origin must be an exact HTTPS origin");
    }
    this.stateProvider = options.stateProvider;
  }
  connections = /* @__PURE__ */ new Map();
  accepted = /* @__PURE__ */ new Map();
  applyChain = Promise.resolve();
  runSequence = 0;
  disposed = false;
  stateProvider;
  get capabilities() {
    return Object.freeze({
      protocol: "ato.activity-experience@1",
      interaction_protocol: "ato.browser@1",
      bridge_version: BROWSER_RUNNER_BRIDGE_VERSION,
      input_profile: BROWSER_RUNNER_INPUT_PROFILE,
      trusted_input: false,
      remote_multi_actor: true,
      state_observation: Boolean(this.stateProvider)
    });
  }
  identity() {
    return {
      activity_id: this.options.activity_id,
      run_id: this.options.run_id,
      runner_instance_id: this.options.runner_instance_id
    };
  }
  registerStateProvider(provider) {
    if (this.disposed) throw new Error("runner_unavailable");
    this.stateProvider = provider;
    return () => {
      if (this.stateProvider === provider) this.stateProvider = void 0;
    };
  }
  async attach(transport, attach) {
    if (this.disposed || this.connections.has(transport.id)) {
      transport.close("runner_unavailable");
      return false;
    }
    try {
      validateAttach(attach, this.identity(), this.options.expected_origin);
      const authority = await this.options.validateCapability(
        attach.capability,
        this.identity()
      );
      validateAuthority(
        authority,
        this.identity(),
        this.options.now?.() ?? Date.now()
      );
      this.connections.set(transport.id, {
        transport,
        authority,
        lastClientSequence: 0
      });
      transport.send({
        type: "ACTOR_ATTACHED",
        actor_id: authority.actor_id,
        controller_session_id: authority.controller_session_id,
        controller_epoch: authority.controller_epoch,
        observe: authority.observe,
        interact: authority.interact
      });
      markBrowserRunner("runner_ready_at", {
        actor_id: authority.actor_id,
        connection_id: transport.id
      });
      return true;
    } catch (error) {
      transport.send({
        type: "REJECT",
        code: rejectCode(error, "capability_rejected")
      });
      transport.close("attachment_rejected");
      return false;
    }
  }
  receive(connectionId, value) {
    const connection = this.connections.get(connectionId);
    if (!connection || this.disposed) return Promise.resolve();
    const operationId = operationIdFrom(value);
    let operation;
    try {
      operation = parseOperation(value);
    } catch {
      connection.transport.send({
        type: "REJECT",
        ...operationId ? { operation_id: operationId } : {},
        code: "invalid_message"
      });
      return Promise.resolve();
    }
    markBrowserRunner("operation_received_at", {
      connection_id: connectionId,
      operation_id: operation.operation_id
    });
    this.applyChain = this.applyChain.then(
      () => this.apply(connection, operation),
      () => this.apply(connection, operation)
    );
    return this.applyChain;
  }
  serve(transport, runnerNonce = createRunnerNonce()) {
    let attached = false;
    transport.start(
      (message) => {
        if (!attached) {
          void this.attach(transport, message).then(
            (accepted) => {
              attached = accepted;
            }
          );
          return;
        }
        void this.receive(transport.id, message);
      },
      () => this.detach(transport.id),
      () => {
        markBrowserRunner("bridge_hello_at", this.identity());
        transport.send({
          type: "BRIDGE_HELLO",
          protocol: "ato.activity-experience@1",
          interaction_protocol: "ato.browser@1",
          runtime: "browser_document",
          bridge_version: BROWSER_RUNNER_BRIDGE_VERSION,
          capabilities: this.capabilities,
          runner_nonce: runnerNonce,
          ...this.identity()
        });
      }
    );
  }
  detach(connectionId, reason = "peer_disconnected") {
    const connection = this.connections.get(connectionId);
    if (!connection) return;
    this.connections.delete(connectionId);
    connection.transport.close(reason);
  }
  dispose(reason = "runner_host_left") {
    if (this.disposed) return;
    this.disposed = true;
    for (const connection of this.connections.values()) {
      connection.transport.send({ type: "RUNNER_UNAVAILABLE", reason });
      connection.transport.close(reason);
    }
    this.connections.clear();
  }
  async apply(connection, operation) {
    const { authority, transport } = connection;
    if (operation.actor_id && operation.actor_id !== authority.actor_id) {
      this.reject(transport, operation.operation_id, "actor_spoof");
      return;
    }
    if (!authority.interact) {
      this.reject(transport, operation.operation_id, "interaction_denied");
      return;
    }
    const fingerprint = operationFingerprint(operation, authority.actor_id);
    const duplicate = this.accepted.get(operation.operation_id);
    if (duplicate) {
      if (duplicate.fingerprint !== fingerprint) {
        this.reject(transport, operation.operation_id, "operation_id_conflict");
        return;
      }
      transport.send(duplicate.ack);
      if (duplicate.observation && authority.observe) {
        transport.send(duplicate.observation);
      }
      return;
    }
    if (operation.client_seq <= connection.lastClientSequence) {
      this.reject(transport, operation.operation_id, "stale_client_sequence");
      return;
    }
    try {
      await this.options.applyOperation(operation.payload, authority);
    } catch {
      this.reject(transport, operation.operation_id, "operation_failed");
      return;
    }
    connection.lastClientSequence = operation.client_seq;
    const runSeq = ++this.runSequence;
    const ack = {
      type: "ACK",
      operation_id: operation.operation_id,
      actor_id: authority.actor_id,
      run_seq: runSeq,
      status: "applied"
    };
    markBrowserRunner("operation_applied_at", {
      actor_id: authority.actor_id,
      operation_id: operation.operation_id,
      run_seq: runSeq
    });
    let projection;
    try {
      projection = this.stateProvider?.();
    } catch {
      projection = void 0;
    }
    const observation = projection ? {
      type: "STATE_OBSERVATION",
      run_seq: runSeq,
      state_revision: projection.revision,
      state: projection.summary
    } : void 0;
    this.remember(operation.operation_id, { fingerprint, ack, observation });
    transport.send(ack);
    this.broadcast({
      type: "APPLIED_RECEIPT",
      operation_id: ack.operation_id,
      actor_id: ack.actor_id,
      run_seq: ack.run_seq,
      status: ack.status
    });
    if (observation) this.broadcast(observation, true);
  }
  remember(operationId, accepted) {
    this.accepted.set(operationId, accepted);
    if (this.accepted.size <= MAX_DEDUPE_ENTRIES) return;
    const oldest = this.accepted.keys().next().value;
    if (oldest) this.accepted.delete(oldest);
  }
  broadcast(message, observersOnly = false) {
    for (const connection of this.connections.values()) {
      if (!observersOnly || connection.authority.observe) {
        connection.transport.send(message);
      }
    }
  }
  reject(transport, operationId, code) {
    transport.send({ type: "REJECT", operation_id: operationId, code });
  }
}
function markBrowserRunner(name, detail) {
  try {
    performance.mark(name, { detail });
  } catch {
  }
}
class MessagePortRunnerConnection {
  constructor(id, port) {
    this.id = id;
    this.port = port;
  }
  send(message) {
    this.port.postMessage(message);
  }
  start(onMessage, onClose, onOpen) {
    this.port.addEventListener("message", (event) => onMessage(event.data));
    this.port.addEventListener("messageerror", onClose, { once: true });
    this.port.start();
    onOpen();
  }
  close() {
    this.port.close();
  }
}
class DataChannelRunnerConnection {
  constructor(id, channel) {
    this.id = id;
    this.channel = channel;
    if (channel.label !== BROWSER_RUNNER_DATA_CHANNEL_LABEL || channel.ordered !== true) {
      throw new TypeError("invalid Browser Runner DataChannel");
    }
  }
  send(message) {
    if (this.channel.readyState === "open") {
      this.channel.send(JSON.stringify(message));
    }
  }
  start(onMessage, onClose, onOpen) {
    this.channel.addEventListener("message", (event) => {
      if (typeof event.data !== "string") return onMessage(null);
      try {
        onMessage(JSON.parse(event.data));
      } catch {
        onMessage(null);
      }
    });
    this.channel.addEventListener("close", onClose, { once: true });
    this.channel.addEventListener("error", onClose, { once: true });
    if (this.channel.readyState === "open") onOpen();
    else this.channel.addEventListener("open", onOpen, { once: true });
  }
  close() {
    this.channel.close();
  }
}
function validateAttach(attach, expected, expectedOrigin) {
  if (!attach || attach.type !== "ATTACH_ACTOR")
    throw new Error("invalid_message");
  if (attach.expected_origin !== expectedOrigin)
    throw new Error("origin_mismatch");
  if (attach.activity_id !== expected.activity_id || attach.run_id !== expected.run_id || attach.runner_instance_id !== expected.runner_instance_id) {
    throw new Error("identity_mismatch");
  }
  if (typeof attach.capability !== "string" || attach.capability.length < 32) {
    throw new Error("capability_rejected");
  }
}
function validateAuthority(authority, expected, now) {
  validateIdentity(authority);
  for (const value of [authority.actor_id, authority.controller_session_id]) {
    if (!ID$1.test(value)) throw new Error("capability_rejected");
  }
  if (authority.activity_id !== expected.activity_id || authority.run_id !== expected.run_id || authority.runner_instance_id !== expected.runner_instance_id) {
    throw new Error("identity_mismatch");
  }
  if (!Number.isSafeInteger(authority.controller_epoch) || authority.controller_epoch <= 0) {
    throw new Error("capability_rejected");
  }
  const issuedAt = Date.parse(authority.issued_at);
  const expiresAt = Date.parse(authority.expires_at);
  if (!Number.isFinite(issuedAt) || !Number.isFinite(expiresAt) || issuedAt > now + 3e4 || expiresAt <= now) {
    throw new Error("capability_rejected");
  }
}
function validateIdentity(identity) {
  for (const value of [
    identity.activity_id,
    identity.run_id,
    identity.runner_instance_id
  ]) {
    if (!ID$1.test(value)) throw new TypeError("invalid Browser Runner identity");
  }
}
function parseOperation(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("invalid_message");
  }
  const operation = value;
  if (operation.type !== "OPERATION" || typeof operation.operation_id !== "string" || !ID$1.test(operation.operation_id) || !Number.isSafeInteger(operation.client_seq) || operation.client_seq <= 0 || operation.protocol_id !== "ato.browser@1" || typeof operation.kind !== "string" || operation.kind !== operation.payload?.type || operation.actor_id !== void 0 && typeof operation.actor_id !== "string" || new TextEncoder().encode(JSON.stringify(value)).byteLength > MAX_MESSAGE_BYTES) {
    throw new Error("invalid_message");
  }
  validateBrowserEvent(operation.payload);
  return value;
}
function validateBrowserEvent(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("invalid_message");
  }
  const event = value;
  if (event.type === "keyboard") {
    const modifiers = event.modifiers;
    if (!["key_down", "key_up"].includes(String(event.kind)) || typeof event.code !== "string" || event.code.length === 0 || event.code.length > 64 || !modifiers || typeof modifiers !== "object" || Array.isArray(modifiers) || !hasExactBooleanKeys(modifiers, ["alt", "control", "meta", "shift"])) {
      throw new Error("invalid_message");
    }
    return;
  }
  if (event.type === "click") {
    validateNormalized(event.x_normalized);
    validateNormalized(event.y_normalized);
    if (!Number.isSafeInteger(event.button)) throw new Error("invalid_message");
    return;
  }
  if (event.type === "scroll") {
    if (!Number.isFinite(event.x) || !Number.isFinite(event.y)) {
      throw new Error("invalid_message");
    }
    return;
  }
  if (event.type === "pointer") {
    if (![
      "pointer_down",
      "pointer_up",
      "pointer_cancel",
      "pointer_move"
    ].includes(String(event.kind)) || !Number.isSafeInteger(event.pointer_id) || !["mouse", "pen"].includes(String(event.pointer_type)) || !Number.isSafeInteger(event.button) || !Number.isSafeInteger(event.buttons)) {
      throw new Error("invalid_message");
    }
    validateNormalized(event.x_normalized);
    validateNormalized(event.y_normalized);
    return;
  }
  throw new Error("invalid_message");
}
function hasExactBooleanKeys(value, keys) {
  return Object.keys(value).length === keys.length && keys.every((key) => typeof value[key] === "boolean");
}
function createRunnerNonce() {
  const bytes = crypto.getRandomValues(new Uint8Array(24));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/gu, "-").replace(/\//gu, "_").replace(/=+$/gu, "");
}
function validateNormalized(value) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0 || value > 1) {
    throw new Error("invalid_message");
  }
}
function operationFingerprint(operation, actorId) {
  return JSON.stringify({
    actor_id: actorId,
    client_seq: operation.client_seq,
    kind: operation.kind,
    payload: operation.payload,
    protocol_id: operation.protocol_id
  });
}
function operationIdFrom(value) {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return void 0;
  const operationId = value.operation_id;
  return typeof operationId === "string" && ID$1.test(operationId) ? operationId : void 0;
}
function normalizeOrigin(value) {
  try {
    const url = new URL(value);
    const local = url.hostname === "localhost" || url.hostname === "127.0.0.1";
    if (url.username || url.password || url.pathname !== "/" || url.search || url.hash || url.protocol !== "https:" && !(url.protocol === "http:" && local)) {
      return null;
    }
    return url.origin;
  } catch {
    return null;
  }
}
function rejectCode(error, fallback) {
  const value = error instanceof Error ? error.message : "";
  return [
    "identity_mismatch",
    "origin_mismatch",
    "capability_rejected"
  ].includes(value) ? value : fallback;
}
const EXPERIENCE_PROTOCOL = "ato.activity-experience@1";
const ID = /^[A-Za-z0-9_-]{8,160}$/;
const CHANNEL_TOKEN = /^[A-Za-z0-9_-]{32,128}$/;
function createAtoBrowserBridge(options) {
  return new AtoBrowserDocumentBridge(options);
}
class AtoBrowserDocumentBridge {
  constructor(options) {
    this.options = options;
    const allowedControllerOrigins = validatedOrigins(
      options.allowedControllerOrigins
    );
    this.allowedVerifierOrigins = validatedOrigins(
      options.allowedCapabilityVerifierOrigins
    );
    this.identity = parseWindowPeerIdentity(
      window.location.hash,
      allowedControllerOrigins
    );
    const peerWindow = window.parent !== window ? window.parent : window.opener;
    if (!peerWindow) throw new Error("controller_window_unavailable");
    this.peerWindow = peerWindow;
    this.pendingStateProvider = options.stateProvider;
    this.createPeerConnection = options.createPeerConnection ?? ((configuration) => new RTCPeerConnection(configuration));
    window.addEventListener("message", this.receiveWindowMessage);
    window.addEventListener("pagehide", this.dispose, { once: true });
    this.post("experience.hello", {
      capabilities: [
        EXPERIENCE_PROTOCOL,
        "ato.browser@1",
        "browser_dom",
        "remote_multi_actor"
      ],
      run_id: this.identity.runId
    });
  }
  identity;
  peerWindow;
  allowedVerifierOrigins;
  createPeerConnection;
  domAdapter = new BrowserDomOperationAdapter();
  media = /* @__PURE__ */ new Map();
  outboundSequence = 0;
  inboundSequence = 0;
  state = "loading";
  runner = null;
  pendingStateProvider;
  disposed = false;
  registerStateProvider(provider) {
    this.pendingStateProvider = provider;
    const unregister = this.runner?.registerStateProvider(provider);
    return () => {
      unregister?.();
      if (this.pendingStateProvider === provider) {
        this.pendingStateProvider = void 0;
      }
    };
  }
  dispose = () => {
    if (this.disposed) return;
    this.disposed = true;
    window.removeEventListener("message", this.receiveWindowMessage);
    for (const current of this.media.values()) {
      current.peer.close();
    }
    this.media.clear();
    this.runner?.dispose("runner_host_left");
    this.runner = null;
    this.state = "ended";
  };
  receiveWindowMessage = (event) => {
    if (event.origin !== this.identity.parentOrigin || event.source !== this.peerWindow || this.disposed) {
      return;
    }
    const envelope = parseExperienceEnvelope(event.data, this.identity);
    if (!envelope || envelope.sequence <= this.inboundSequence) {
      this.post("experience.error", { code: "invalid_command" });
      return;
    }
    this.inboundSequence = envelope.sequence;
    void this.handle(envelope, event.ports).catch(() => {
      this.post("experience.error", { code: "invalid_command" });
    });
  };
  async handle(envelope, ports) {
    switch (envelope.type) {
      case "experience.configure":
        await this.configure(envelope.payload);
        return;
      case "experience.browser-runner.connect":
        requireExactKeys(envelope.payload, []);
        if (!this.runner || ports.length !== 1) {
          throw new Error("runner_unavailable");
        }
        this.runner.serve(
          new MessagePortRunnerConnection(
            `local_${this.identity.instanceId}`,
            ports[0]
          )
        );
        return;
      case "experience.resume":
        requireExactKeys(envelope.payload, []);
        if (this.state !== "paused") throw new Error("invalid_state");
        this.setState("running");
        return;
      case "experience.pause":
        requireExactKeys(envelope.payload, []);
        if (this.state !== "running") throw new Error("invalid_state");
        this.setState("paused");
        return;
      case "experience.media.start":
        await this.startMedia(envelope.payload);
        return;
      case "experience.media.answer":
        await this.answerMedia(envelope.payload);
        return;
      case "experience.media.ice":
        await this.addMediaIce(envelope.payload);
        return;
      case "experience.media.stop":
        requireExactKeys(envelope.payload, [
          "media_generation",
          "peer_id",
          "publisher_connection_id",
          "run_id",
          "subscriber_participant_id"
        ]);
        this.stopMedia(parseMediaSignal(envelope.payload));
        return;
      case "experience.dispose":
        requireExactKeys(envelope.payload, []);
        this.dispose();
        return;
      default:
        throw new Error("unknown_command");
    }
  }
  async configure(payload) {
    requireExactKeys(payload, ["browser_runner", "run_id"]);
    if (this.state !== "loading" || payload.run_id !== this.identity.runId) {
      throw new Error("identity_mismatch");
    }
    const config = record(payload.browser_runner);
    requireExactKeys(config, [
      "activity_id",
      "capability_verifier_url",
      "runner_instance_id"
    ]);
    if (typeof config.activity_id !== "string" || !ID.test(config.activity_id) || typeof config.runner_instance_id !== "string" || !ID.test(config.runner_instance_id) || typeof config.capability_verifier_url !== "string") {
      throw new Error("invalid_configuration");
    }
    const verifierUrl = validatedVerifierUrl(
      config.capability_verifier_url,
      this.allowedVerifierOrigins
    );
    const identity = {
      activity_id: config.activity_id,
      run_id: this.identity.runId,
      runner_instance_id: config.runner_instance_id
    };
    this.runner = new BrowserRunnerBridge({
      ...identity,
      expected_origin: this.identity.parentOrigin,
      validateCapability: (capability, expected) => verifyCapability(verifierUrl, capability, expected),
      applyOperation: this.options.applyOperation ?? ((event) => {
        if (this.state !== "running") throw new Error("runner_paused");
        this.domAdapter.apply(event);
      }),
      stateProvider: this.pendingStateProvider
    });
    this.setState("paused");
    this.post("experience.ready", { run_id: this.identity.runId });
  }
  async startMedia(payload) {
    requireExactKeys(payload, [
      "ice_servers",
      "media_generation",
      "peer_id",
      "publisher_connection_id",
      "run_id",
      "subscriber_participant_id"
    ]);
    const signal = parseMediaSignal(payload);
    this.requireRun(signal);
    const iceServers = payload.ice_servers;
    if (!Array.isArray(iceServers)) throw new Error("invalid_media");
    this.stopMedia(signal);
    if (!this.runner) throw new Error("runner_unavailable");
    const peer = this.createPeerConnection({
      iceServers
    });
    const current = { signal, peer, pendingIce: [] };
    this.media.set(signal.peer_id, current);
    const channel = peer.createDataChannel(BROWSER_RUNNER_DATA_CHANNEL_LABEL, {
      ordered: true
    });
    this.runner.serve(
      new DataChannelRunnerConnection(
        `remote_${signal.subscriber_participant_id}_${signal.peer_id}`,
        channel
      )
    );
    peer.onicecandidate = (event) => {
      if (!this.isCurrentMedia(current)) return;
      this.post("experience.media.ice", {
        ...signal,
        candidate: event.candidate?.toJSON() ?? null
      });
    };
    peer.onconnectionstatechange = () => {
      if (!this.isCurrentMedia(current)) return;
      this.post("experience.media.state", {
        ...signal,
        state: peer.connectionState
      });
      if (peer.connectionState === "failed" || peer.connectionState === "closed") {
        this.stopMedia(signal);
      }
    };
    const offer = await peer.createOffer();
    await peer.setLocalDescription(offer);
    if (!this.isCurrentMedia(current) || !peer.localDescription?.sdp) return;
    this.post("experience.media.offer", {
      ...signal,
      sdp: peer.localDescription.sdp
    });
  }
  async answerMedia(payload) {
    requireExactKeys(payload, [
      "media_generation",
      "peer_id",
      "publisher_connection_id",
      "run_id",
      "sdp",
      "subscriber_participant_id"
    ]);
    const signal = parseMediaSignal(payload);
    this.requireRun(signal);
    if (typeof payload.sdp !== "string" || payload.sdp.length === 0) {
      throw new Error("invalid_media");
    }
    const current = this.requireMedia(signal);
    await current.peer.setRemoteDescription({
      type: "answer",
      sdp: payload.sdp
    });
    for (const candidate of current.pendingIce.splice(0)) {
      await current.peer.addIceCandidate(candidate);
    }
  }
  async addMediaIce(payload) {
    requireExactKeys(payload, [
      "candidate",
      "media_generation",
      "peer_id",
      "publisher_connection_id",
      "run_id",
      "subscriber_participant_id"
    ]);
    const signal = parseMediaSignal(payload);
    this.requireRun(signal);
    const candidate = parseIce(payload.candidate);
    const current = this.requireMedia(signal);
    if (!current.peer.remoteDescription) {
      current.pendingIce.push(candidate);
      return;
    }
    await current.peer.addIceCandidate(candidate);
  }
  stopMedia(signal) {
    this.requireRun(signal);
    const current = this.media.get(signal.peer_id);
    if (!current || !sameMediaSignal(current.signal, signal)) return;
    this.media.delete(signal.peer_id);
    current.peer.close();
  }
  requireMedia(signal) {
    const current = this.media.get(signal.peer_id);
    if (!current || !sameMediaSignal(current.signal, signal)) {
      throw new Error("media_identity_mismatch");
    }
    return current;
  }
  requireRun(signal) {
    if (signal.run_id !== this.identity.runId) {
      throw new Error("media_identity_mismatch");
    }
  }
  isCurrentMedia(current) {
    return this.media.get(current.signal.peer_id) === current;
  }
  setState(state) {
    this.state = state;
    this.post("experience.state", { state });
  }
  post(type, payload) {
    this.peerWindow.postMessage(
      {
        protocol: EXPERIENCE_PROTOCOL,
        channelToken: this.identity.channelToken,
        instanceId: this.identity.instanceId,
        sequence: ++this.outboundSequence,
        type,
        payload
      },
      this.identity.parentOrigin
    );
  }
}
class BrowserDomOperationAdapter {
  pointerTargets = /* @__PURE__ */ new Map();
  apply(event) {
    if (event.type === "keyboard") {
      const target2 = document.activeElement ?? document.body;
      target2.dispatchEvent(
        new KeyboardEvent(event.kind === "key_down" ? "keydown" : "keyup", {
          bubbles: true,
          cancelable: true,
          code: event.code,
          key: keyForCode(event.code),
          altKey: event.modifiers.alt,
          ctrlKey: event.modifiers.control,
          metaKey: event.modifiers.meta,
          shiftKey: event.modifiers.shift
        })
      );
      return;
    }
    if (event.type === "scroll") {
      window.scrollBy({ left: event.x, top: event.y, behavior: "instant" });
      return;
    }
    const clientX = event.x_normalized * window.innerWidth;
    const clientY = event.y_normalized * window.innerHeight;
    if (event.type === "click") {
      const target2 = document.elementFromPoint(clientX, clientY);
      if (!target2) throw new Error("target_unavailable");
      if (target2 instanceof HTMLElement) target2.focus({ preventScroll: true });
      target2.dispatchEvent(
        new MouseEvent("click", {
          bubbles: true,
          cancelable: true,
          clientX,
          clientY,
          button: event.button
        })
      );
      return;
    }
    const target = this.pointerTargets.get(event.pointer_id) ?? document.elementFromPoint(clientX, clientY);
    if (!target) throw new Error("target_unavailable");
    const type = event.kind.replace("_", "");
    if (event.kind === "pointer_down") {
      this.pointerTargets.set(event.pointer_id, target);
    }
    target.dispatchEvent(
      new PointerEvent(type, {
        bubbles: true,
        cancelable: true,
        pointerId: event.pointer_id,
        pointerType: event.pointer_type,
        clientX,
        clientY,
        button: event.button,
        buttons: event.buttons
      })
    );
    if (event.kind === "pointer_up" || event.kind === "pointer_cancel") {
      this.pointerTargets.delete(event.pointer_id);
    }
  }
}
class BrowserDomTextObservationAdapter {
  operations = new BrowserDomOperationAdapter();
  revision = 0;
  lastActor = null;
  apply(event, authority) {
    this.operations.apply(event);
    this.revision += 1;
    this.lastActor = authority.actor_id;
  }
  project() {
    return {
      revision: this.revision,
      summary: {
        projection: "dom_text",
        text: visibleDocumentText(),
        last_actor: this.lastActor
      }
    };
  }
}
function visibleDocumentText() {
  return (document.body?.innerText ?? document.body?.textContent ?? "").replace(/\s+/gu, " ").trim().slice(0, 8192);
}
async function verifyCapability(verifierUrl, capability, identity) {
  const response = await fetch(verifierUrl, {
    method: "POST",
    headers: {
      authorization: `BrowserRunnerPeer ${capability}`,
      "content-type": "application/json"
    },
    body: JSON.stringify(identity),
    credentials: "omit",
    cache: "no-store",
    referrerPolicy: "no-referrer"
  });
  if (!response.ok) throw new Error("capability_rejected");
  const value = await response.json();
  return record(value.claims);
}
function parseWindowPeerIdentity(hash, allowedOrigins) {
  const params = new URLSearchParams(hash.replace(/^#/u, ""));
  const parentOrigin = exactOrigin(params.get("parent_origin"));
  const channelToken = params.get("channel_token") ?? "";
  const instanceId = params.get("instance_id") ?? "";
  const runId = params.get("run_id") ?? "";
  const seat = params.get("seat_id");
  if (params.size !== 5 || !parentOrigin || !allowedOrigins.has(parentOrigin) || !CHANNEL_TOKEN.test(channelToken) || !ID.test(instanceId) || !ID.test(runId) || seat !== "0" && seat !== "1") {
    throw new Error("invalid_window_identity");
  }
  return {
    parentOrigin,
    channelToken,
    instanceId,
    runId,
    seat: seat === "0" ? 0 : 1
  };
}
function parseExperienceEnvelope(value, identity) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const envelope = value;
  if (Object.keys(envelope).length !== 6 || envelope.protocol !== EXPERIENCE_PROTOCOL || envelope.channelToken !== identity.channelToken || envelope.instanceId !== identity.instanceId || !Number.isSafeInteger(envelope.sequence) || envelope.sequence <= 0 || typeof envelope.type !== "string" || !envelope.payload || typeof envelope.payload !== "object" || Array.isArray(envelope.payload)) {
    return null;
  }
  return value;
}
function parseMediaSignal(value) {
  const keys = [
    "run_id",
    "peer_id",
    "media_generation",
    "publisher_connection_id",
    "subscriber_participant_id"
  ];
  for (const key of keys.filter((key2) => key2 !== "media_generation")) {
    if (typeof value[key] !== "string" || !ID.test(value[key])) {
      throw new Error("invalid_media");
    }
  }
  if (!Number.isSafeInteger(value.media_generation) || value.media_generation <= 0) {
    throw new Error("invalid_media");
  }
  return {
    run_id: value.run_id,
    peer_id: value.peer_id,
    media_generation: value.media_generation,
    publisher_connection_id: value.publisher_connection_id,
    subscriber_participant_id: value.subscriber_participant_id
  };
}
function parseIce(value) {
  if (value === null) return null;
  const candidate = record(value);
  if (typeof candidate.candidate !== "string") {
    throw new Error("invalid_media");
  }
  return candidate;
}
function sameMediaSignal(left, right) {
  return left.run_id === right.run_id && left.peer_id === right.peer_id && left.media_generation === right.media_generation && left.publisher_connection_id === right.publisher_connection_id && left.subscriber_participant_id === right.subscriber_participant_id;
}
function validatedOrigins(values) {
  const result = /* @__PURE__ */ new Set();
  for (const value of values) {
    const origin = exactOrigin(value);
    if (!origin || origin !== value)
      throw new TypeError("invalid_origin_policy");
    result.add(origin);
  }
  if (result.size === 0) throw new TypeError("empty_origin_policy");
  return result;
}
function validatedVerifierUrl(value, allowedOrigins) {
  const url = new URL(value);
  if (!allowedOrigins.has(url.origin) || url.username || url.password || url.hash || url.search || url.pathname !== "/v1/browser-runner/peer-capability/verify") {
    throw new Error("invalid_verifier_url");
  }
  return url.toString();
}
function exactOrigin(value) {
  if (!value) return null;
  try {
    const url = new URL(value);
    const local = url.hostname === "localhost" || url.hostname === "127.0.0.1";
    if (url.username || url.password || url.pathname !== "/" || url.search || url.hash || url.protocol !== "https:" && !(url.protocol === "http:" && local)) {
      return null;
    }
    return url.origin;
  } catch {
    return null;
  }
}
function requireExactKeys(value, keys) {
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new Error("invalid_message");
  }
}
function record(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("invalid_message");
  }
  return value;
}
function keyForCode(code) {
  if (code.startsWith("Key") && code.length === 4) return code.slice(3);
  if (code.startsWith("Digit") && code.length === 6) return code.slice(5);
  return code;
}
const controllerPolicy = readOriginPolicy(
  "ato-browser-runner-controller-origins"
);
const verifierPolicy = readOriginPolicy("ato-browser-runner-verifier-origins");
if (controllerPolicy.length > 0 && verifierPolicy.length > 0) {
  const observation = readPolicy("ato-browser-runner-state-observation") === "dom_text" ? new BrowserDomTextObservationAdapter() : null;
  window.atoBrowserBridge = createAtoBrowserBridge({
    allowedControllerOrigins: controllerPolicy,
    allowedCapabilityVerifierOrigins: verifierPolicy,
    ...observation ? {
      applyOperation: (event, authority) => observation.apply(event, authority),
      stateProvider: () => observation.project()
    } : {}
  });
}
function readOriginPolicy(name) {
  const content = readPolicy(name);
  return content ? content.split(",").map((value) => value.trim()).filter(Boolean) : [];
}
function readPolicy(name) {
  return document.querySelector(`meta[name="${name}"]`)?.content.trim() ?? "";
}
export {
  BROWSER_RUNNER_BRIDGE_VERSION,
  BROWSER_RUNNER_DATA_CHANNEL_LABEL,
  BROWSER_RUNNER_INPUT_PROFILE,
  BrowserDomOperationAdapter,
  BrowserDomTextObservationAdapter,
  BrowserRunnerBridge,
  DataChannelRunnerConnection,
  MessagePortRunnerConnection,
  createAtoBrowserBridge
};
