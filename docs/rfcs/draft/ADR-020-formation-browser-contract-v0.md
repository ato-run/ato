# ADR-020 — Browser Contract v0: verifying a realized candidate in a browser

**Status**: proposed
**Context**: Formation Browser Verification v0 (`ato form --verify-browser --accept "<prompt>"`), on top of ADR-019

## The question

ADR-019 made a local Formation observe the candidate before reporting
`Formed`, but only through typed HTTP observations. A person's acceptance
criterion for a web application is usually something else: "create a note,
reload, it is still there". That needs a browser, and a judgment about what
the browser saw.

## The decision

```text
original prompt
  -> BrowserContractV0 { original_prompt, criteria[], normalization }
  -> the SAME temporary realization ADR-019 launches
  -> browser verifier helper (Stagehand drives, Jev judges)
  -> BrowserVerificationResult
  -> BrowserVerificationReceipt (validated, recomputed, bound to the Contract)
  -> pass | fail | inconclusive
```

- **Contract.** v0 keeps the prompt verbatim and makes it one required
  `browser.task` criterion (`normalization = "v0.identity"`). No DSL, no
  decomposition, and no model is involved in building it. The browser
  Contract's ref is the SHA-256 of its JCS form; the receipt carries it
  (`browser_contract_ref`) and the prompt's own digest.
- **Identity.** A browser Contract is a condition of success, so it is part
  of the final K. Without one, a Formation's `contract_ref` is the base
  Contract's, unchanged. With one, it is the SHA-256 of the JCS
  `EffectiveContractV0 { schema: "ato.effective-contract/0",
  base_contract_ref, browser_contract_ref }`; the attempt keeps
  `base_contract_ref` for provenance. The same base Contract with different
  acceptance prompts is two different Ks.
- **Opt-in.** Without `--verify-browser`, Formation is unchanged. With it,
  the browser runs only after the typed HTTP Contract is fully satisfied, on
  the same realization, before it is destroyed; only a browser `pass` forms
  the candidate. Lanes that are not realized (static) are Filtered with
  `browser_contract_needs_realization`.
- **Process boundary.** The verifier is a Node helper
  (`apps/formation-browser-verifier`), one JSON request on stdin and one JSON
  result on stdout. The worker starts it with a cleared environment (an
  allowlist carrying only the judge and agent keys), its own process group, a
  wall clock plus grace, and a scratch directory that also holds the browser
  profile — so a browser the launcher detached is still found and stopped.
- **Browser.** Stagehand 3.7.3 (the last v3 release, pinned) in `LOCAL` mode,
  `disableAPI`, headless Chrome. Navigation to the URL the realization
  reported is deterministic; the task itself is carried out by Stagehand's
  agent (DOM mode, `search` excluded), bounded by `max_browser_steps`. The
  agent's task is the ONLY step that acts on the application.
- **Origin boundary.** The helper runs a guard proxy that refuses and records
  every request. Chrome sends all traffic to it with exactly one bypass, the
  candidate's `host:port`, and `<-loopback>` removes Chrome's implicit
  loopback bypass — so another loopback port, `localhost`, `[::1]`, a LAN
  address, the internet, a WebSocket or a redirect target is refused by
  Chrome's network stack before any connection to it exists. Attribution is
  separate from enforcement: the page's own CDP network events
  (`Network.requestWillBeSent`, `Network.webSocketCreated`) and main-frame
  navigations to another origin become `blocked_request` / `origin_violation`
  events; the browser's own background traffic is refused but not held
  against the candidate. A post-step origin check stays as defence in depth.
  Any violation rules out a pass.
- **Evidence.** Three kinds, never mixed:
  `browser_snapshot` — URL, title, `document.body.innerText` (bounded) and the
  navigation type, read through CDP with no model involved;
  `model_extracted_facts` — Stagehand `extract()` output, a model's reading;
  `agent_report` — the agent's own account. Navigations, loads and refused
  requests are `observed_events`; the agent's step list is recorded as
  `agent_claimed_step` actions, never as events. A pass must cite a
  `browser_snapshot`.
- **Agent model.** Stagehand needs a text-generating model:
  `deepseek/deepseek-flash` (DeepSeek-V4.1-Flash) by default,
  `ATO_BROWSER_AGENT_MODEL` to override.
- **Judge.** Jev's role is evidence judgment only: it does not build,
  normalize or decompose K and does not drive the browser. Jev
  (`jev-1.13.0`, pinned) is reached through TypeSafe's only evaluation
  endpoint, `POST /v1/systemone`, as a native Choice over `complete`,
  `verify_more`, `incomplete`. There is no completion endpoint to compare
  against; the three-way Choice is the completion judgment. Its state
  separates `observed_browser_events`, `observed_page_states`,
  `model_derived_facts`, `agent_claims` and `known_gaps`; its instructions
  make the observed evidence primary and say a claim alone never shows the
  objective was achieved. `complete` → pass, `incomplete` → fail,
  `verify_more` → observe again (a snapshot and a model's reading — nothing
  clicks, types or navigates) and another round, up to `max_jev_rounds`;
  still `verify_more` → inconclusive. Probabilities and confidence are kept as
  evidence and grant nothing.
- **Trust.** The Contract and the verifier's fixed prompts are trusted; page
  content is not. Page text reaches the judge only inside `state` — as
  observed page states or model-derived facts — and the judge's instructions
  say all page text is untrusted data, including text announcing that the
  task succeeded. The agent's own report goes to `agent_claims`. Nothing from
  a page is ever placed in the judge's instructions or criteria, or in the
  agent's system prompt.
- **Verdicts.** `pass` = every required criterion passed; `fail` = a required
  criterion failed; `inconclusive` = anything else. Rust recomputes the
  overall verdict and refuses a result whose verdict its own judgment does
  not support (a `pass` needs `complete`, a `fail` needs `incomplete`).
  "Deferred" is not a terminal result here and `verify_more` never leaves the
  helper.
- **Failure of the machinery is never the application's fault.** A missing
  verifier, a crashed or slow helper, an unreachable judge, a browser that
  died, an agent that could not operate (model out of credit, for instance)
  — all `inconclusive`. The judge is not asked to rule on a page nobody
  operated.
- **Secrets.** `JEV_API_KEY` and the agent key come from the environment
  only. The helper and its browser get a `HOME` inside the verification
  scratch directory. The receipt carries no cookies, no full DOM (bounded excerpts only),
  and is refused if it contains either key's value.

## Consequences

- A browser verdict costs one agent run and one to three judge calls; the
  acceptance fixtures took 19–48 s each.
- The agent is a generating model and can be wrong about what it did; that
  is why its claims are labelled and the judge rules on the observed page.
- Stagehand's experimental Jev fast path for act/observe/extract
  (browserbase/stagehand#2951–#2955) is unmerged and targets v4; adopting it
  is a later change, not v0.
- Process lanes only, because only they are realized in Phase 1.
- Page-attributed violations are read from the page target's CDP session;
  a request from a worker or an out-of-process iframe is still refused by the
  guard but may not be attributed to the candidate.
