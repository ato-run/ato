import React, { useState, useEffect, useCallback, useRef } from "react";

// ── parseSummary ────────────────────────────────────────────────────────────
function parseSummary(raw) {
  const r = {
    network: [], readOnly: [], readWrite: [], secrets: [],
    runtime: "", driver: "", target: "", capsule: "",
    policyHash: "", provisioningHash: "", command: "", workingDir: "",
  };
  if (!raw) return r;
  for (const line of raw.split("\n")) {
    const m = line.match(/^([^:]+):\s*(.*)/);
    if (!m) continue;
    const key = m[1].trim().toLowerCase().replace(/\s+/g, "_");
    const val = m[2].trim();
    switch (key) {
      case "network":
      case "network_ids":
        r.network = val && val !== "None" ? val.split(/,\s*/).filter(Boolean) : [];
        break;
      case "read_only":   r.readOnly  = val && val !== "None" ? val.split(/,\s*/).filter(Boolean) : []; break;
      case "read_write":  r.readWrite = val && val !== "None" ? val.split(/,\s*/).filter(Boolean) : []; break;
      case "secrets":     r.secrets   = val && val !== "None" ? val.split(/,\s*/).filter(Boolean) : []; break;
      case "policy_hash": r.policyHash = val; break;
      case "provisioning": r.provisioningHash = val; break;
      case "target": {
        const tm = val.match(/^(\S+)\s*(?:\((.+)\))?/);
        if (tm) {
          r.target = tm[1];
          if (tm[2]) for (const p of tm[2].split(/,\s*/)) {
            const pm = p.match(/(\w+)=(\S+)/);
            if (pm) {
              if (pm[1] === "runtime") r.runtime = pm[2];
              if (pm[1] === "driver") r.driver = pm[2];
            }
          }
        } else r.target = val;
        break;
      }
      case "capsule": r.capsule = val; break;
      case "command": r.command = val; break;
      case "working_directory":
      case "working_dir": r.workingDir = val; break;
      default: break;
    }
  }
  return r;
}

// ── IPC bridge ──────────────────────────────────────────────────────────────
function bridge(cmd) {
  const msg = JSON.stringify({ capsule: "launch", command: cmd });
  if (window.ipc && window.ipc.postMessage) window.ipc.postMessage(msg);
  else console.log("[no bridge]", cmd);
}

// ── SVG icons (inline) ──────────────────────────────────────────────────────
const ShieldIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" style={{ width: 22, height: 22 }}>
    <path d="M12 2l8 4v6c0 5-3.5 9-8 10-4.5-1-8-5-8-10V6l8-4z"/>
    <polyline points="9 12 11 14 15 10"/>
  </svg>
);

const GlobeIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ width: 16, height: 16 }}>
    <circle cx="12" cy="12" r="10"/>
    <line x1="2" y1="12" x2="22" y2="12"/>
    <path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/>
  </svg>
);

const FolderIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ width: 16, height: 16 }}>
    <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>
  </svg>
);

const KeyIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ width: 16, height: 16 }}>
    <circle cx="8" cy="15" r="4"/>
    <line x1="11.3" y1="11.3" x2="19" y2="4"/>
    <line x1="16" y1="7" x2="18" y2="9"/>
  </svg>
);

const ChevronRight = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" style={{ width: 14, height: 14 }}>
    <polyline points="9 18 15 12 9 6"/>
  </svg>
);

const XIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" style={{ width: 16, height: 16 }}>
    <line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/>
  </svg>
);

// ── PermRow ─────────────────────────────────────────────────────────────────
function PermRow({ icon, title, desc, subdesc, onEdit, highlight }) {
  return (
    <div style={{
      display: "flex", alignItems: "flex-start", gap: 12,
      padding: "12px 0", borderBottom: "1px solid var(--border-soft)",
    }}>
      <div style={{
        width: 32, height: 32, borderRadius: 8, display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0,
        background: highlight ? "var(--accent-light)" : "var(--surface-2)",
        color: highlight ? "var(--accent)" : "var(--muted)",
      }}>
        {icon}
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 2 }}>
          <span style={{ fontWeight: 600, fontSize: 13 }}>{title}</span>
          {onEdit && (
            <button
              onClick={onEdit}
              style={{
                background: "none", border: "none", cursor: "pointer",
                fontSize: 11, color: "var(--accent)", padding: "2px 6px",
                borderRadius: 4, fontWeight: 500,
              }}
            >Edit</button>
          )}
        </div>
        <div style={{ fontSize: 12, color: "var(--muted)", lineHeight: 1.5 }}>{desc}</div>
        {subdesc && <div style={{ fontSize: 11, color: "var(--soft)", marginTop: 2 }}>{subdesc}</div>}
      </div>
    </div>
  );
}

// ── ConfigRow ────────────────────────────────────────────────────────────────
function ConfigRow({ field, onEdit }) {
  let valueText = "";
  let isEmpty = false;
  if (field.kind === "secret") {
    if (field.value) valueText = field.value.slice(0, 3) + "•".repeat(Math.max(6, Math.min(field.value.length - 3, 9)));
    else if (field.already_configured) valueText = "sk-" + "•".repeat(9);
    else { valueText = "Not set"; isEmpty = true; }
  } else if (field.kind === "enum") {
    valueText = field.value || field.choices?.[0] || "—";
  } else {
    if (field.value) valueText = field.value;
    else if (field.already_configured) valueText = "(configured)";
    else { valueText = "Not set"; isEmpty = true; }
  }

  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 0", borderBottom: "1px solid var(--border-soft)" }}>
      <span style={{ flex: 1, fontSize: 12, color: "var(--muted)" }}>{field.label}</span>
      <span style={{ fontSize: 12, color: isEmpty ? "var(--danger)" : "var(--text)", fontWeight: isEmpty ? 400 : 500, minWidth: 80, textAlign: "right" }}>{valueText}</span>
      <button onClick={onEdit} style={{ background: "none", border: "none", cursor: "pointer", fontSize: 11, color: "var(--accent)", padding: "2px 6px", borderRadius: 4, fontWeight: 500 }}>Edit</button>
    </div>
  );
}

// ── EditConfigPanel ──────────────────────────────────────────────────────────
function EditConfigPanel({ fields, onFieldChange, onClose }) {
  const [revealed, setRevealed] = useState({});

  return (
    <div>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 16, paddingBottom: 12, borderBottom: "1px solid var(--border-soft)" }}>
        <div>
          <div style={{ fontWeight: 700, fontSize: 14 }}>Edit Configuration</div>
          <div style={{ fontSize: 12, color: "var(--muted)", marginTop: 2 }}>Update the required settings for this capsule.</div>
        </div>
        <button onClick={onClose} style={{ background: "none", border: "none", cursor: "pointer", color: "var(--muted)", padding: 4, borderRadius: 6 }}><XIcon /></button>
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
        {fields.map((f, i) => (
          <div key={f.name}>
            <div style={{ fontSize: 12, fontWeight: 600, marginBottom: 6, color: "var(--text)" }}>{f.label}</div>
            {f.kind === "enum" ? (
              <select
                value={f.value || f.choices?.[0] || ""}
                onChange={e => onFieldChange(i, e.target.value)}
                style={{ width: "100%", padding: "8px 10px", borderRadius: 8, border: "1px solid var(--border-soft)", fontSize: 13, background: "var(--surface)", color: "var(--text)", outline: "none" }}
              >
                {(f.choices || []).map(c => <option key={c} value={c}>{c}</option>)}
              </select>
            ) : (
              <div style={{ display: "flex", gap: 6 }}>
                <input
                  type={f.kind === "secret" && !revealed[i] ? "password" : "text"}
                  value={f.value || ""}
                  placeholder={f.placeholder || (f.kind === "secret" ? "••••••••" : "")}
                  onChange={e => onFieldChange(i, e.target.value)}
                  style={{ flex: 1, padding: "8px 10px", borderRadius: 8, border: "1px solid var(--border-soft)", fontSize: 13, background: "var(--surface)", color: "var(--text)", outline: "none" }}
                />
                {f.kind === "secret" && (
                  <button
                    type="button"
                    onClick={() => setRevealed(r => ({ ...r, [i]: !r[i] }))}
                    style={{ padding: "0 10px", borderRadius: 8, border: "1px solid var(--border-soft)", background: "var(--surface)", fontSize: 12, color: "var(--muted)", cursor: "pointer" }}
                  >{revealed[i] ? "Hide" : "Reveal"}</button>
                )}
              </div>
            )}
            {f.kind === "secret" && (
              <div style={{ fontSize: 11, color: "var(--muted)", marginTop: 4 }}>Used only for {f.label} requests.</div>
            )}
          </div>
        ))}
        <div style={{ background: "var(--surface-2)", borderRadius: 8, padding: "10px 12px", fontSize: 11, color: "var(--muted)", lineHeight: 1.6 }}>
          These values are stored only for this capsule. Changing the model may affect app behavior.
        </div>
      </div>
      <div style={{ display: "flex", gap: 8, marginTop: 20, paddingTop: 12, borderTop: "1px solid var(--border-soft)" }}>
        <button onClick={onClose} style={{ flex: 1, padding: "9px 0", borderRadius: 8, border: "1px solid var(--border-soft)", background: "var(--surface)", fontSize: 13, cursor: "pointer" }}>Cancel</button>
        <button onClick={onClose} style={{ flex: 1, padding: "9px 0", borderRadius: 8, border: "none", background: "var(--accent)", color: "#fff", fontSize: 13, fontWeight: 600, cursor: "pointer" }}>Save changes</button>
      </div>
    </div>
  );
}

// ── NetworkPanel ──────────────────────────────────────────────────────────────
function NetworkPanel({ hosts, onHostsChange, onClose }) {
  const addHost = () => {
    const h = prompt("Enter hostname or IP (e.g. api.example.com):");
    if (h && h.trim()) onHostsChange([...hosts, h.trim()]);
  };
  return (
    <div>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 16, paddingBottom: 12, borderBottom: "1px solid var(--border-soft)" }}>
        <div>
          <div style={{ fontWeight: 700, fontSize: 14 }}>Edit Network Access</div>
          <div style={{ fontSize: 12, color: "var(--muted)", marginTop: 2 }}>Choose which hosts this capsule can connect to.</div>
        </div>
        <button onClick={onClose} style={{ background: "none", border: "none", cursor: "pointer", color: "var(--muted)", padding: 4, borderRadius: 6 }}><XIcon /></button>
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 6, minHeight: 60 }}>
        {hosts.length === 0 && <div style={{ color: "var(--muted)", fontSize: 12, padding: "8px 0" }}>No hosts allowed</div>}
        {hosts.map(h => (
          <div key={h} style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 10px", background: "var(--surface)", borderRadius: 8, border: "1px solid var(--border-soft)" }}>
            <span style={{ width: 7, height: 7, borderRadius: "50%", background: "var(--ok)", flexShrink: 0 }} />
            <span style={{ flex: 1, fontSize: 13 }}>{h}</span>
            <button onClick={() => onHostsChange(hosts.filter(x => x !== h))} style={{ background: "none", border: "none", cursor: "pointer", color: "var(--muted)", padding: 2 }}><XIcon /></button>
          </div>
        ))}
      </div>
      <div style={{ display: "flex", gap: 8, marginTop: 16, paddingTop: 12, borderTop: "1px solid var(--border-soft)" }}>
        <button onClick={addHost} style={{ flex: 1, padding: "9px 0", borderRadius: 8, border: "1px dashed var(--border-soft)", background: "var(--surface)", fontSize: 13, cursor: "pointer", color: "var(--accent)" }}>+ Add host</button>
        <button onClick={onClose} style={{ flex: 1, padding: "9px 0", borderRadius: 8, border: "none", background: "var(--accent)", color: "#fff", fontSize: 13, fontWeight: 600, cursor: "pointer" }}>Done</button>
      </div>
    </div>
  );
}

// ── TechDetails ──────────────────────────────────────────────────────────────
function TechDetails({ sd, consent }) {
  const c = consent?.[0] || {};
  const items = [
    ["Runtime", sd.runtime || "—"], ["Secrets", (sd.secrets || []).join(", ") || "None"],
    ["Driver", sd.driver || "—"], ["Policy hash", c.policy_segment_hash || sd.policyHash || "—"],
    ["Target", sd.target || c.target_label || "—"], ["Provisioning hash", c.provisioning_policy_hash || sd.provisioningHash || "—"],
    ["Network mode", sd.network?.length > 0 ? "allow listed hosts only" : "blocked"], ["Working directory", sd.workingDir || "—"],
    ["Read access", (sd.readOnly || []).join(", ") || "none"], ["Command", sd.command || "—"],
    ["Write access", (sd.readWrite || []).join(", ") || "none"],
  ];
  return (
    <div style={{
      display: "grid", gridTemplateColumns: "1fr 1fr", gap: 6,
      background: "var(--surface-2)", borderRadius: 8, padding: 12, marginTop: 8,
    }}>
      {items.map(([label, value]) => (
        <div key={label} style={{ fontSize: 11, color: "var(--muted)" }}>
          <span style={{ fontWeight: 600, marginRight: 4 }}>{label}{label ? ":" : ""}</span>
          <span style={{ color: "var(--text)" }} title={value}>{value.length > 30 ? value.slice(0, 30) + "…" : value}</span>
        </div>
      ))}
    </div>
  );
}

// ── ConsentScreen (main) ─────────────────────────────────────────────────────
export function ConsentScreen() {
  const [preview, setPreview] = useState(() => window.__ATO_LAUNCH_PREVIEW ?? null);
  const [cfgFields, setCfgFields] = useState([]);
  const [consent, setConsent] = useState([]);
  const [sd, setSd] = useState(null);
  const [panel, setPanel] = useState(null); // null | 'config' | 'network' | 'fs' | 'secrets'
  const [approved, setApproved] = useState(false);
  const [showTech, setShowTech] = useState(false);
  const [networkHosts, setNetworkHosts] = useState([]);

  const processPreview = useCallback((p) => {
    setPreview(p);
    if (!p || p.preflight_failed) return;

    const fields = [];
    const cons = [];
    let parsedSd = null;

    (p.requirements || []).forEach(req => {
      if (req.type === "secrets_required") {
        (req.fields || []).forEach(f => {
          const defaultVal = (f.kind === "enum" && (f.default || f.choices?.[0]))
            ? (f.default || f.choices[0]) : "";
          fields.push({
            name: f.name, label: f.label || f.name, kind: f.kind || "text",
            choices: f.choices || [], placeholder: f.placeholder || "",
            already_configured: !!f.already_configured, value: defaultVal,
          });
        });
      } else if (req.type === "consent_required") {
        cons.push(req);
        if (!parsedSd && req.summary) parsedSd = parseSummary(req.summary);
      }
    });

    setCfgFields(fields);
    setConsent(cons);
    setSd(parsedSd);
    setNetworkHosts(parsedSd?.network ?? []);
  }, []);

  useEffect(() => {
    const p = window.__ATO_LAUNCH_PREVIEW;
    if (p && !p.loading) processPreview(p);
    window.__ato_hydrate_preview = processPreview;
    return () => { delete window.__ato_hydrate_preview; };
  }, [processPreview]);

  useEffect(() => {
    const onKey = (e) => {
      if (e.key === "Escape") {
        if (panel) setPanel(null);
        else bridge({ kind: "cancel" });
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [panel]);

  const updateField = (i, value) => {
    setCfgFields(prev => prev.map((f, idx) => idx === i ? { ...f, value } : f));
  };

  const allFilled = cfgFields.every(f => f.kind !== "secret" || f.already_configured || !!f.value);
  const canApprove = !!(preview && !preview.loading && !preview.preflight_failed && approved && allFilled);

  const handleApprove = () => {
    if (!preview || preview.loading) return;
    const secrets = {}, config = {};
    cfgFields.forEach(f => {
      if (!f.value) return;
      if (f.kind === "secret") secrets[f.name] = f.value;
      else config[f.name] = f.value;
    });
    const consents = consent.map(r => ({
      scoped_id: r.scoped_id, version: r.version,
      target_label: r.target_label,
      policy_segment_hash: r.policy_segment_hash,
      provisioning_policy_hash: r.provisioning_policy_hash,
    }));
    bridge({ kind: "approve", preview_id: preview.preview_id, secrets, config, consents });
  };

  const isLoading = !preview || preview.loading;

  const appInfoFields = !isLoading && preview ? [
    { label: "Application", value: preview.name || "—" },
    { label: "Capsule ID",  value: preview.capsule_id || "—" },
    { label: "Handle",     value: preview.handle || "—" },
    { label: "Target",     value: (preview.visited_targets || []).join(", ") || "—" },
    { label: "Version",    value: preview.capsule_version || "—" },
  ] : [];

  const hasSd = !!(sd || consent.length > 0);
  const sdSafe = sd || { network: [], readOnly: [], readWrite: [], secrets: [] };

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100vh", background: "var(--bg)", fontFamily: "var(--font-system)", fontSize: 13, color: "var(--text)" }}>
      {/* Backdrop */}
      {panel && (
        <div
          onClick={() => setPanel(null)}
          style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.3)", zIndex: 10, backdropFilter: "blur(2px)" }}
        />
      )}

      {/* Side panel */}
      {panel && (
        <div style={{
          position: "fixed", right: 0, top: 0, bottom: 0,
          width: 320, background: "var(--bg)", borderLeft: "1px solid var(--border-soft)",
          padding: 20, overflowY: "auto", zIndex: 20,
          boxShadow: "-4px 0 20px rgba(0,0,0,0.12)",
        }}>
          {panel === "config" && (
            <EditConfigPanel
              fields={cfgFields}
              onFieldChange={updateField}
              onClose={() => setPanel(null)}
            />
          )}
          {panel === "network" && (
            <NetworkPanel
              hosts={networkHosts}
              onHostsChange={h => { setNetworkHosts(h); setSd(prev => prev ? { ...prev, network: h } : prev); }}
              onClose={() => setPanel(null)}
            />
          )}
          {(panel === "fs" || panel === "secrets") && (
            <div>
              <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 16, paddingBottom: 12, borderBottom: "1px solid var(--border-soft)" }}>
                <div style={{ fontWeight: 700, fontSize: 14 }}>{panel === "fs" ? "Filesystem Access" : "Secrets"}</div>
                <button onClick={() => setPanel(null)} style={{ background: "none", border: "none", cursor: "pointer", color: "var(--muted)", padding: 4 }}><XIcon /></button>
              </div>
              {panel === "fs" ? (
                <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
                  <div>
                    <div style={{ fontSize: 11, fontWeight: 600, color: "var(--muted)", marginBottom: 4, textTransform: "uppercase", letterSpacing: "0.05em" }}>Read access</div>
                    <div style={{ fontSize: 13, padding: "8px 10px", background: "var(--surface)", borderRadius: 8, border: "1px solid var(--border-soft)" }}>
                      {sdSafe.readOnly.join(", ") || "None"}
                    </div>
                  </div>
                  <div>
                    <div style={{ fontSize: 11, fontWeight: 600, color: "var(--muted)", marginBottom: 4, textTransform: "uppercase", letterSpacing: "0.05em" }}>Write access</div>
                    <div style={{ fontSize: 13, padding: "8px 10px", background: "var(--surface)", borderRadius: 8, border: "1px solid var(--border-soft)" }}>
                      {sdSafe.readWrite.join(", ") || "None"}
                    </div>
                  </div>
                </div>
              ) : (
                <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                  {sdSafe.secrets.length === 0 ? (
                    <div style={{ color: "var(--muted)", fontSize: 12 }}>No secrets required.</div>
                  ) : sdSafe.secrets.map(s => (
                    <div key={s} style={{ padding: "8px 0", borderBottom: "1px solid var(--border-soft)", fontSize: 13 }}>
                      {s.replace(/_/g, " ").replace(/\b\w/g, c => c.toUpperCase())}
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      )}

      {/* Scroll area */}
      <div style={{ flex: 1, overflowY: "auto", padding: "24px 20px 0" }}>
        {/* Header */}
        <div style={{ textAlign: "center", marginBottom: 24 }}>
          <div style={{
            width: 44, height: 44, borderRadius: 12, background: "var(--accent-light)",
            color: "var(--accent)", display: "flex", alignItems: "center", justifyContent: "center", margin: "0 auto 12px",
          }}>
            <ShieldIcon />
          </div>
          <h1 style={{ margin: 0, fontSize: 18, fontWeight: 700 }}>Review Before Launch</h1>
          <p style={{ margin: "6px 0 0", fontSize: 13, color: "var(--muted)" }}>Review what this capsule can access before it runs.</p>
        </div>

        {/* Loading */}
        {isLoading && (
          <div style={{ textAlign: "center", padding: "40px 0", color: "var(--muted)", display: "flex", alignItems: "center", justifyContent: "center", gap: 10 }}>
            <div style={{ width: 18, height: 18, border: "2px solid var(--border-soft)", borderTopColor: "var(--accent)", borderRadius: "50%", animation: "spin 0.8s linear infinite" }} />
            <span>Verifying capsule...</span>
          </div>
        )}

        {/* Preflight error */}
        {!isLoading && preview?.preflight_failed && (
          <div style={{ background: "#fff5f5", border: "1px solid #fecaca", borderRadius: 10, padding: 16, marginBottom: 16 }}>
            <div style={{ fontWeight: 700, color: "var(--danger)", marginBottom: 4 }}>⚠ Capsule verification failed</div>
            <p style={{ margin: 0, fontSize: 12, color: "var(--muted)" }}>Could not retrieve launch information. Please cancel and try again.</p>
            {preview.preflight_error && (
              <pre style={{ margin: "10px 0 8px", fontSize: 11, background: "var(--surface)", padding: 8, borderRadius: 6, overflowX: "auto", whiteSpace: "pre-wrap", wordBreak: "break-all" }}>
                {preview.preflight_error}
              </pre>
            )}
          </div>
        )}

        {/* Ready state */}
        {!isLoading && !preview?.preflight_failed && (
          <>
            {/* App info grid */}
            <div style={{ background: "var(--surface)", borderRadius: 10, padding: "4px 0", marginBottom: 16, border: "1px solid var(--border-soft)" }}>
              {appInfoFields.map(({ label, value }) => (
                <div key={label} style={{ display: "flex", padding: "8px 14px", gap: 12, borderBottom: "1px solid var(--border-soft)", lastChild: { borderBottom: "none" } }}>
                  <span style={{ fontSize: 12, color: "var(--muted)", width: 90, flexShrink: 0 }}>{label}</span>
                  <span style={{ fontSize: 12, fontWeight: 500, color: "var(--text)", flex: 1, wordBreak: "break-all" }}>{value}</span>
                </div>
              ))}
            </div>

            {/* Config section */}
            {cfgFields.length > 0 && (
              <>
                <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 10, fontSize: 11, fontWeight: 700, color: "var(--muted)", textTransform: "uppercase", letterSpacing: "0.06em" }}>
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ width: 14, height: 14 }}>
                    <rect x="3" y="11" width="18" height="10" rx="2"/>
                    <path d="M7 11V7a5 5 0 0 1 10 0v4"/>
                  </svg>
                  Required Configuration
                </div>
                <div style={{ borderRadius: 10, border: "1px solid var(--border-soft)", overflow: "hidden", marginBottom: 16 }}>
                  {cfgFields.map((f, i) => (
                    <div key={f.name} style={{ padding: "0 14px", borderBottom: i < cfgFields.length - 1 ? "1px solid var(--border-soft)" : "none" }}>
                      <ConfigRow field={f} onEdit={() => setPanel("config")} />
                    </div>
                  ))}
                </div>
              </>
            )}

            {/* Permissions section */}
            {hasSd && (
              <>
                <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 10, fontSize: 11, fontWeight: 700, color: "var(--muted)", textTransform: "uppercase", letterSpacing: "0.06em" }}>
                  <ShieldIcon style={{ width: 14, height: 14 }} />
                  Permissions
                </div>
                <div style={{ borderRadius: 10, border: "1px solid var(--border-soft)", padding: "0 14px", marginBottom: 8 }}>
                  <PermRow
                    icon={<GlobeIcon />}
                    title="Network"
                    highlight={networkHosts.length > 0}
                    desc={networkHosts.length > 0
                      ? networkHosts.slice(0, 3).join(", ") + (networkHosts.length > 3 ? `, +${networkHosts.length - 3} more` : "")
                      : "No network access"}
                    subdesc={networkHosts.length > 0 ? "All other hosts are blocked by default." : null}
                    onEdit={() => setPanel("network")}
                  />
                  <PermRow
                    icon={<FolderIcon />}
                    title="Filesystem"
                    highlight={sdSafe.readOnly.length > 0 || sdSafe.readWrite.length > 0}
                    desc={`Read access: ${sdSafe.readOnly.join(", ") || "None"}  •  Write access: ${sdSafe.readWrite.join(", ") || "None"}`}
                    onEdit={() => setPanel("fs")}
                  />
                  <PermRow
                    icon={<KeyIcon />}
                    title="Secrets"
                    highlight={sdSafe.secrets.length > 0}
                    desc={sdSafe.secrets.length > 0
                      ? `Uses ${sdSafe.secrets.map(s => s.replace(/_/g, " ").replace(/\b\w/g, c => c.toUpperCase())).join(", ")}, available only to this capsule.`
                      : "No secrets required"}
                    onEdit={() => setPanel("secrets")}
                  />
                </div>

                {/* Tech details toggle */}
                <button
                  onClick={() => setShowTech(v => !v)}
                  style={{
                    display: "flex", alignItems: "center", gap: 6, padding: "8px 0",
                    background: "none", border: "none", cursor: "pointer",
                    fontSize: 12, color: "var(--muted)", fontWeight: 500, marginBottom: 4,
                  }}
                >
                  <ChevronRight style={{ transform: showTech ? "rotate(90deg)" : "none", transition: "transform 0.15s" }} />
                  {showTech ? "Hide technical details" : "Show technical details"}
                </button>
                {showTech && <TechDetails sd={sdSafe} consent={consent} />}
              </>
            )}

            {/* Approval checkbox */}
            <div
              onClick={() => setApproved(v => !v)}
              style={{
                display: "flex", alignItems: "flex-start", gap: 12,
                padding: 14, borderRadius: 10, marginTop: 16, marginBottom: 8,
                background: approved ? "var(--accent-light)" : "var(--surface)",
                border: `1px solid ${approved ? "var(--accent-border)" : "var(--border-soft)"}`,
                cursor: "pointer", transition: "all 0.15s",
              }}
            >
              <input
                type="checkbox"
                checked={approved}
                onChange={() => {}}
                onClick={e => e.stopPropagation()}
                style={{ marginTop: 1, cursor: "pointer", accentColor: "var(--accent)" }}
              />
              <div>
                <div style={{ fontWeight: 600, fontSize: 13 }}>I allow this capsule to run with the permissions shown above.</div>
                <div style={{ fontSize: 12, color: "var(--muted)", marginTop: 2 }}>You can review and change these settings anytime in Ato Desktop.</div>
                {approved && !allFilled && (
                  <div style={{ fontSize: 11, color: "var(--danger)", marginTop: 4 }}>Fill in all required configuration fields above to continue.</div>
                )}
              </div>
            </div>
          </>
        )}
      </div>

      {/* Footer */}
      <div style={{ padding: "12px 20px", display: "flex", gap: 8, borderTop: "1px solid var(--border-soft)", background: "var(--bg)" }}>
        <button
          onClick={() => bridge({ kind: "cancel" })}
          style={{ flex: 1, padding: "9px 0", borderRadius: 8, border: "1px solid var(--border-soft)", background: "var(--surface)", fontSize: 13, cursor: "pointer", fontWeight: 500, color: "var(--text)" }}
        >Cancel</button>
        <button
          onClick={handleApprove}
          disabled={!canApprove}
          style={{
            flex: 2, padding: "9px 0", borderRadius: 8, border: "none",
            background: canApprove ? "var(--accent)" : "var(--surface-2)",
            color: canApprove ? "#fff" : "var(--soft)",
            fontSize: 13, fontWeight: 600, cursor: canApprove ? "pointer" : "not-allowed",
            transition: "all 0.15s",
          }}
        >Allow Permissions & Launch</button>
      </div>

      <style>{`
        @keyframes spin { to { transform: rotate(360deg); } }
      `}</style>
    </div>
  );
}
