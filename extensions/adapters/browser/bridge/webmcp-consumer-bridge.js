(() => {
  "use strict";

  // This script deliberately runs in the page's main world before producer
  // scripts. It contains no Ato credential. Everything it reads is untrusted.
  const requestEvent = "__ato_webmcp_request_v1";
  const responseEvent = "__ato_webmcp_response_v1";
  const registry = new Map();
  const installed = new WeakSet();
  const inFlight = new Map();
  const arrayIsArray = Array.isArray.bind(Array);
  const stringify = JSON.stringify.bind(JSON);
  const getOwnPropertyDescriptor = Object.getOwnPropertyDescriptor.bind(Object);
  const getOwnPropertyDescriptors = Object.getOwnPropertyDescriptors.bind(Object);
  const documentToken = cryptoToken();
  const maxTools = 256;
  const maxSchemaBytes = 16 * 1024;
  const maxSchemaDepth = 8;
  const maxSchemaNodes = 512;
  // Code-unit bounds are intentionally conservative: one UTF-16 code unit
  // can expand to four UTF-8 bytes in the Rust/CDP boundary.
  const maxRegistryCodeUnits = 40 * 1024;
  const maxObservationCodeUnits = 8 * 1024;
  let producerApi = "unavailable";
  let registryGeneration = 1;

  installKnownProducers();
  document.addEventListener(requestEvent, receiveRequest, false);

  const consumer = Object.freeze({
    snapshot: async () => {
      await refreshCurrentRegistry();
      return {
        document_token: documentToken,
        producer_api: producerApi,
        registry_generation: registryGeneration,
        origin: globalThis.location.origin,
        tools: [...registry.values()].map(({ definition }) => definition),
        untrusted_observation: fixtureObservation(),
      };
    },
    identity: () => ({ document_token: documentToken, origin: globalThis.location.origin }),
    abortActive: () => {
      if (inFlight.size !== 1) return false;
      const controller = inFlight.values().next().value;
      controller.abort();
      return true;
    },
    abort: (operationId) => {
      if (typeof operationId !== "string") return false;
      const controller = inFlight.get(operationId);
      if (!controller) return false;
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
          const normalizedName = normalizeOperationName(name);
          if (normalizedName && registry.delete(normalizedName)) registryGeneration += 1;
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
      if (!arrayIsArray(tools)) return;
      producerApi = context === document.modelContext
        ? "document_model_context"
        : "deprecated_alias";
      const registrations = [];
      for (let index = 0; index < Math.min(tools.length, maxTools); index += 1) {
        try {
          const raw = tools[index];
          const producerName = raw?.name;
          const definition = normalizeDefinition(raw);
          if (!definition || typeof producerName !== "string") continue;
          const invoke = typeof context.invokeTool === "function"
            ? (argumentsValue, signal) => context.invokeTool(producerName, argumentsValue, { signal })
            : typeof context.executeTool === "function"
              ? (argumentsValue, signal) => context.executeTool(producerName, argumentsValue, { signal })
              : null;
          registrations.push({ definition, invoke, owner: "native_probe" });
        } catch {
          // One hostile definition must not suppress unrelated operations.
        }
      }
      replaceOwnedRegistrations("native_probe", boundedRegistrations(registrations));
    } catch {
      // An unstable native consumer failure means no newly discovered tools.
    }
  }

  function refreshFixture() {
    const tools = globalThis.__ATO_WEBMCP_FIXTURE_TOOLS__;
    if (!arrayIsArray(tools)) return;
    producerApi = "deterministic_fixture_polyfill";
    const registrations = [];
    for (let index = 0; index < Math.min(tools.length, maxTools); index += 1) {
      try {
        const tool = tools[index];
        const definition = normalizeDefinition(tool);
        if (!definition) continue;
        const handler = tool.execute ?? tool.handler ?? tool.invoke;
        registrations.push({
          definition,
          invoke: typeof handler === "function"
            ? (argumentsValue, signal) => handler(argumentsValue, { signal })
            : null,
          owner: "fixture",
        });
      } catch {
        // Drop only the malformed fixture definition.
      }
    }
    replaceOwnedRegistrations("fixture", boundedRegistrations(registrations));
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
      owner: "native_probe",
    };
  }

  function replaceRegistration(registration, handlerChangeInvalidates = true) {
    const previous = registry.get(registration.definition.name);
    if (previous && registration.owner === "native_probe" && registration.invoke === null) {
      registration = { ...registration, invoke: previous.invoke, owner: previous.owner };
    }
    const nextSignature = operationalSignature(registration.definition);
    const definitionChanged = !previous ||
      operationalSignature(previous.definition) !== nextSignature;
    if (definitionChanged || (handlerChangeInvalidates && previous.invoke !== registration.invoke)) {
      registry.set(registration.definition.name, registration);
      registryGeneration += 1;
    } else if (previous && previous.invoke !== registration.invoke) {
      registry.set(registration.definition.name, registration);
    }
  }

  function replaceOwnedRegistrations(owner, registrations) {
    const next = new Map(registrations.map((registration) => [registration.definition.name, registration]));
    let operationallyChanged = false;
    for (const [name, registration] of [...registry]) {
      if (registration.owner === owner && !next.has(name)) {
        registry.delete(name);
        operationallyChanged = true;
      }
    }
    for (let registration of next.values()) {
      const previous = registry.get(registration.definition.name);
      if (previous && registration.invoke === null) {
        registration = { ...registration, invoke: previous.invoke };
      }
      if (!previous || operationalSignature(previous.definition) !==
          operationalSignature(registration.definition)) {
        operationallyChanged = true;
      }
      // Handler closures produced by listTools/getTools are deliberately not
      // identity. The newest closure is retained without epoch churn.
      registry.set(registration.definition.name, registration);
    }
    if (operationallyChanged) registryGeneration += 1;
  }

  function boundedRegistrations(registrations) {
    const bounded = [];
    let units = 0;
    for (const registration of registrations) {
      const encoded = stringify(registration.definition);
      if (units + encoded.length > maxRegistryCodeUnits) continue;
      units += encoded.length;
      bounded.push(registration);
    }
    return bounded;
  }

  function normalizeDefinition(raw) {
    try {
      if (!isObject(raw)) return null;
      const name = normalizeOperationName(raw.name);
      if (!name) return null;
      const schemaInput = isObject(raw.inputSchema)
        ? raw.inputSchema
        : isObject(raw.input_schema)
          ? raw.input_schema
          : {};
      const inputSchema = boundedJsonClone(schemaInput, maxSchemaBytes);
      if (inputSchema === null || !isObject(inputSchema)) return null;
      return {
        name,
        description: boundedString(raw.description, 1024),
        input_schema: inputSchema,
        // Page output is neither operation identity nor trusted evidence.
        output: null,
        origin: boundedString(raw.origin, 2048) ?? globalThis.location.origin,
        read_only: raw.readOnly === true || raw.read_only === true,
      };
    } catch {
      return null;
    }
  }

  function operationalSignature(definition) {
    return stringify({
      name: definition.name,
      input_schema: definition.input_schema,
      origin: definition.origin,
      read_only: definition.read_only,
    });
  }

  function boundedString(value, maximum) {
    return typeof value === "string" && value.length <= maximum ? value : null;
  }

  function boundedJsonClone(value, maximumBytes) {
    const seen = new WeakSet();
    const state = { nodes: 0 };
    const clone = visit(value, 0);
    if (clone === undefined) return null;
    const encoded = stringify(clone);
    return encoded.length <= maximumBytes ? clone : null;

    function visit(current, depth) {
      if (current === null || typeof current === "boolean") return current;
      if (typeof current === "number") return Number.isFinite(current) ? current : undefined;
      if (typeof current === "string") return current.length <= 4096 ? current : undefined;
      if (typeof current !== "object" || depth > maxSchemaDepth || seen.has(current) ||
          ++state.nodes > maxSchemaNodes) return undefined;
      seen.add(current);
      if (arrayIsArray(current)) {
        if (current.length > 128) return undefined;
        const output = [];
        for (let index = 0; index < current.length; index += 1) {
          const descriptor = getOwnPropertyDescriptor(current, String(index));
          if (!descriptor || !("value" in descriptor)) return undefined;
          const item = visit(descriptor.value, depth + 1);
          if (item === undefined) return undefined;
          output.push(item);
        }
        return output;
      }
      const descriptors = getOwnPropertyDescriptors(current);
      const keys = Object.keys(descriptors).filter((key) => descriptors[key].enumerable);
      if (keys.length > 128) return undefined;
      const output = Object.create(null);
      for (const key of keys) {
        if (key.length > 128 || !("value" in descriptors[key])) return undefined;
        const item = visit(descriptors[key].value, depth + 1);
        if (item === undefined) return undefined;
        output[key] = item;
      }
      return output;
    }
  }

  function normalizeOperationName(value) {
    if (typeof value !== "string" || value.length === 0 || value.length > 64 ||
        !/^[A-Za-z0-9_-]+$/.test(value)) return null;
    return value.toLowerCase();
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
    if (request.type === "validate_document") {
      await refreshCurrentRegistry();
      const currentGeneration = `${documentToken}.${registryGeneration}`;
      return respond(
        request.id,
        request.realization_generation === currentGeneration,
        request.realization_generation === currentGeneration ? null : "stale_operation",
      );
    }
    if (request.type === "abort") {
      const controller = inFlight.get(request.operation_id);
      if (controller) controller.abort();
      return respond(request.id, true, controller ? null : "operation_not_found");
    }
    if (request.type !== "invoke" || typeof request.operation_name !== "string" ||
        typeof request.realization_generation !== "string" ||
        !Number.isSafeInteger(request.surface_generation) || request.surface_generation <= 0) {
      return respond(request.id, false, "invalid_operation");
    }
    await refreshCurrentRegistry();
    if (request.realization_generation !== `${documentToken}.${registryGeneration}` ||
        request.surface_generation !== registryGeneration) {
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
      detail: stringify({ id, ok, error }),
    }));
  }

  async function refreshCurrentRegistry() {
    installKnownProducers();
    await refreshNativeConsumer();
    if (["unavailable", "deterministic_fixture_polyfill"].includes(producerApi)) {
      refreshFixture();
    }
  }

  function fixtureObservation() {
    const producer = globalThis.__ATO_WEBMCP_FIXTURE_OBSERVATION__;
    if (typeof producer !== "function") return null;
    try { return boundedJsonClone(producer(), maxObservationCodeUnits); } catch { return null; }
  }

  function cryptoToken() {
    if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
    const bytes = crypto.getRandomValues(new Uint8Array(18));
    return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  }

  function isObject(value) {
    return value !== null && typeof value === "object" && !arrayIsArray(value);
  }
})();
