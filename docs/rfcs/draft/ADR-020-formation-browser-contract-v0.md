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
  decomposition. `contract_ref` is the SHA-256 of the JCS form; the receipt
  also carries the prompt's own digest.
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
  `disableAPI`, headless Chrome with a proxy that goes nowhere: only loopback
  — the realized endpoint — is reachable. Navigation to the URL the
  realization reported is deterministic; the task itself is carried out by
  Stagehand's agent (DOM mode, `search` excluded), bounded by
  `max_browser_steps`. Observation is a deterministic snapshot (URL, title,
  page text) plus extracted facts.
- **Agent model.** Stagehand needs a text-generating model:
  `deepseek/deepseek-flash` (DeepSeek-V4.1-Flash) by default,
  `ATO_BROWSER_AGENT_MODEL` to override.
- **Judge.** Jev (`jev-1.13.0`, pinned) through TypeSafe's only evaluation
  endpoint, `POST /v1/systemone`: a native Choice over `complete`,
  `verify_more`, `incomplete`. There is no completion endpoint to compare
  against; the three-way Choice is the completion judgment. `complete` →
  pass, `incomplete` → fail, `verify_more` → a read-only second look and
  another round, up to `max_jev_rounds`; still `verify_more` → inconclusive.
  Probabilities and confidence are kept as evidence and grant nothing.
- **Trust.** The Contract and the verifier's fixed prompts are trusted; page
  content is not. Page text reaches the judge only inside `state`, under a
  field labelled untrusted page content; the agent's own report is recorded as
  "browser agent claimed: …". Nothing from a page is ever placed in the
  judge's instructions or criteria, or in the agent's system prompt.
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
  only. The receipt carries no cookies, no full DOM (bounded excerpts only),
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
- The dead-proxy egress block for the browser is configuration, not a
  measured property in v0's acceptance.
