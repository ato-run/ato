(() => {
  "use strict";

  // This script deliberately runs in the page's main world before producer
  // scripts. It contains no Ato credential. Everything it reads is untrusted.
  const requestEvent = "__ato_webmcp_request_v1";
  const responseEvent = "__ato_webmcp_response_v1";
  const registry = new Map();
  const installed = new WeakSet();
  const inFlight = new Map();
  const documentToken = cryptoToken();
  let producerApi = "unavailable";
  let registryGeneration = 1;
  let fixtureSignature = null;

  installKnownProducers();
  document.addEventListener(requestEvent, receiveRequest, false);

  const consumer = Object.freeze({
    snapshot: async () => {
      installKnownProducers();
      refreshFixture();
      await refreshNativeConsumer();
      return {
        document_token: documentToken,
        producer_api: producerApi,
        registry_generation: registryGeneration,
        origin: globalThis.location.origin,
        tools: [...registry.values()].map(({ definition }) => definition),
        untrusted_observation: fixtureObservation(),
      };
    },
    abortActive: () => {
      if (inFlight.size !== 1) return false;
      const controller = inFlight.values().next().value;
      controller.abort();
      return true;
    },
  });
  Object.defineProperty(globalThis, "__ATO_WEBMCP_CONSUMER__", {
    value: consumer,
    configurable: false,
    enumerable: false,
    writable: false,
  });

  function installKnownProducers() {
    installProducer(document.modelContext, "document_model_context");
    installProducer(globalThis.navigator?.modelContext, "deprecated_alias");
  }

  function installProducer(context, api) {
    if (!isObject(context) || installed.has(context)) return;
    installed.add(context);
    if (typeof context.registerTool !== "function") return;
    producerApi = api;
    const registerTool = context.registerTool.bind(context);
    const unregisterTool = typeof context.unregisterTool === "function"
      ? context.unregisterTool.bind(context)
      : null;
    try {
      context.registerTool = (...args) => {
        const registration = registrationFrom(args, context);
        const result = registerTool(...args);
        if (registration) replaceRegistration(registration);
        return result;
      };
      if (unregisterTool) {
        context.unregisterTool = (name) => {
          const result = unregisterTool(name);
          if (typeof name === "string" && registry.delete(name)) registryGeneration += 1;
          return result;
        };
      }
    } catch {
      // Native objects may expose non-writable methods. The consumer probes
      // below remain isolated here instead of leaking draft API names outward.
    }
  }

  async function refreshNativeConsumer() {
    const context = document.modelContext ?? globalThis.navigator?.modelContext;
    if (!isObject(context)) return;
    const list = typeof context.listTools === "function"
      ? context.listTools.bind(context)
      : typeof context.getTools === "function"
        ? context.getTools.bind(context)
        : null;
    if (!list) return;
    try {
      const tools = await list();
      if (!Array.isArray(tools)) return;
      for (const raw of tools) {
        const definition = normalizeDefinition(raw);
        if (!definition) continue;
        const invoke = typeof context.invokeTool === "function"
          ? (argumentsValue, signal) => context.invokeTool(definition.name, argumentsValue, { signal })
          : typeof context.executeTool === "function"
            ? (argumentsValue, signal) => context.executeTool(definition.name, argumentsValue, { signal })
            : null;
        // A native list probe recreates the closure on every poll. Only a
        // definition change invalidates operation ids in this path.
        replaceRegistration({ definition, invoke }, false);
      }
    } catch {
      // An unstable native consumer failure means no newly discovered tools.
    }
  }

  function refreshFixture() {
    const tools = globalThis.__ATO_WEBMCP_FIXTURE_TOOLS__;
    if (!Array.isArray(tools)) return;
    const definitions = tools.map(normalizeDefinition).filter(Boolean);
    const signature = JSON.stringify(definitions);
    if (signature === fixtureSignature) return;
    fixtureSignature = signature;
    producerApi = "deterministic_fixture_polyfill";
    for (const tool of tools) {
      const definition = normalizeDefinition(tool);
      if (!definition) continue;
      const handler = tool.execute ?? tool.handler ?? tool.invoke;
      replaceRegistration({
        definition,
        invoke: typeof handler === "function"
          ? (argumentsValue, signal) => handler(argumentsValue, { signal })
          : null,
      });
    }
    registryGeneration += 1;
  }

  function registrationFrom(args, context) {
    let raw;
    let handler;
    if (typeof args[0] === "string" && isObject(args[1])) {
      raw = { ...args[1], name: args[0] };
      handler = args[2] ?? args[1].execute ?? args[1].handler ?? args[1].invoke;
    } else if (isObject(args[0])) {
      raw = args[0];
      handler = args[1] ?? raw.execute ?? raw.handler ?? raw.invoke;
    }
    const definition = normalizeDefinition(raw);
    if (!definition) return null;
    return {
      definition,
      invoke: typeof handler === "function"
        ? (argumentsValue, signal) => handler.call(context, argumentsValue, { signal })
        : null,
    };
  }

  function replaceRegistration(registration, handlerChangeInvalidates = true) {
    const previous = registry.get(registration.definition.name);
    const nextSignature = JSON.stringify(registration.definition);
    const definitionChanged = !previous || JSON.stringify(previous.definition) !== nextSignature;
    if (definitionChanged || (handlerChangeInvalidates && previous.invoke !== registration.invoke)) {
      registry.set(registration.definition.name, registration);
      registryGeneration += 1;
    } else if (previous && previous.invoke !== registration.invoke) {
      registry.set(registration.definition.name, registration);
    }
  }

  function normalizeDefinition(raw) {
    if (!isObject(raw) || typeof raw.name !== "string" || raw.name.length === 0) return null;
    return {
      name: raw.name,
      description: typeof raw.description === "string" ? raw.description : null,
      input_schema: isObject(raw.inputSchema)
        ? raw.inputSchema
        : isObject(raw.input_schema)
          ? raw.input_schema
          : {},
      output: raw.output ?? null,
      origin: typeof raw.origin === "string" ? raw.origin : globalThis.location.origin,
      read_only: raw.readOnly === true || raw.read_only === true,
    };
  }

  async function receiveRequest(event) {
    let request;
    try {
      request = JSON.parse(String(event.detail));
    } catch {
      return;
    }
    if (!isObject(request) || typeof request.id !== "string" || typeof request.type !== "string") {
      return;
    }
    if (request.type === "abort") {
      const controller = inFlight.get(request.operation_id);
      if (controller) controller.abort();
      return respond(request.id, true, controller ? null : "operation_not_found");
    }
    if (request.type !== "invoke" || typeof request.operation_name !== "string" ||
        !Number.isSafeInteger(request.surface_generation) || request.surface_generation <= 0) {
      return respond(request.id, false, "invalid_operation");
    }
    installKnownProducers();
    refreshFixture();
    if (request.surface_generation !== registryGeneration) {
      return respond(request.id, false, "stale_operation");
    }
    const registration = registry.get(request.operation_name);
    if (!registration || typeof registration.invoke !== "function") {
      return respond(request.id, false, "unknown_operation");
    }
    const controller = new AbortController();
    inFlight.set(request.operation_id, controller);
    try {
      // Page output is evidence to recover through a fresh observation, never
      // an instruction-bearing response crossing into Ato's isolated world.
      await registration.invoke(request.arguments ?? {}, controller.signal);
      respond(request.id, true, null);
    } catch (error) {
      respond(request.id, false, controller.signal.aborted ? "aborted" : "operation_failed");
    } finally {
      inFlight.delete(request.operation_id);
    }
  }

  function respond(id, ok, error) {
    document.dispatchEvent(new CustomEvent(responseEvent, {
      detail: JSON.stringify({ id, ok, error }),
    }));
  }

  function fixtureObservation() {
    const producer = globalThis.__ATO_WEBMCP_FIXTURE_OBSERVATION__;
    if (typeof producer !== "function") return null;
    try { return producer(); } catch { return null; }
  }

  function cryptoToken() {
    if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
    const bytes = crypto.getRandomValues(new Uint8Array(18));
    return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  }

  function isObject(value) {
    return value !== null && typeof value === "object" && !Array.isArray(value);
  }
})();
