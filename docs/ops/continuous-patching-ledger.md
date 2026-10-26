# Continuous patching ledger

This page moved to [Continuous patching ledger](../archive/2026/ops/continuous-patching-ledger.md).

Status: historical evidence; not current specification or deployment state.
Language of the preserved document: English.

<!-- Compatibility anchors for existing inbound links. -->
<a id="continuous-patching-ledger"></a>
<a id="1-scope-and-the-three-things-that-must-not-be-conflated"></a>
<a id="2-layer-by-layer-current-update-path"></a>
<a id="l0--control-plane"></a>
<a id="l1--worker--runner"></a>
<a id="l2--runtime-images"></a>
<a id="l3--application--oss-source"></a>
<a id="3-formation-worker-lifecycle-exactly-what-is-implemented"></a>
<a id="4-first-validation-case-cve-2026-111497--rustsec-2026-0348"></a>
<a id="5-capsule-identity-rules-for-a-runtime-patch"></a>
<a id="6-pr-split-and-status"></a>
<a id="7-verified-vs-not-verified"></a>
<a id="8-conservative-operating-rule-until-phase-2-lands"></a>
<a id="9-progress-since-this-ledger-2026-10-24-second-batch"></a>
<a id="10-runtime-gate-coverage-and-non-goals-cve-2026-111497"></a>
<a id="101-contract-k-identity-why-a-runtime-patch-does-not-change-k"></a>
<a id="102-what-the-gates-cover"></a>
<a id="103-non-goals-do-not-over-claim"></a>

<!-- Additional Docsify slug aliases (including numeric prefixes). -->
<a id="_1-scope-and-the-three-things-that-must-not-be-conflated"></a>
<a id="_10-runtime-gate-coverage-and-non-goals-cve-2026-111497"></a>
<a id="_101-contract-k-identity-why-a-runtime-patch-does-not-change-k"></a>
<a id="_102-what-the-gates-cover"></a>
<a id="_103-non-goals-do-not-over-claim"></a>
<a id="_2-layer-by-layer-current-update-path"></a>
<a id="_3-formation-worker-lifecycle-exactly-what-is-implemented"></a>
<a id="_4-first-validation-case-cve-2026-111497-rustsec-2026-0348"></a>
<a id="_5-capsule-identity-rules-for-a-runtime-patch"></a>
<a id="_6-pr-split-and-status"></a>
<a id="_7-verified-vs-not-verified"></a>
<a id="_8-conservative-operating-rule-until-phase-2-lands"></a>
<a id="_9-progress-since-this-ledger-2026-10-24-second-batch"></a>
<a id="l0-control-plane"></a>
<a id="l1-worker-runner"></a>
<a id="l2-runtime-images"></a>
<a id="l3-application-oss-source"></a>
