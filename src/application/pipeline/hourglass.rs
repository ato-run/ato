use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) enum HourglassPhase {
    Install,
    Prepare,
    Build,
    Finalize,
    Verify,
    DryRun,
    Execute,
    Publish,
}

impl HourglassPhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Prepare => "prepare",
            Self::Build => "build",
            Self::Finalize => "finalize",
            Self::Verify => "verify",
            Self::DryRun => "dry_run",
            Self::Execute => "execute",
            Self::Publish => "publish",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HourglassPhaseState {
    Run,
    Ok,
    Fail,
    Skip,
}

impl HourglassPhaseState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Run => "RUN",
            Self::Ok => "OK",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HourglassFlow {
    ConsumerRun,
    ProducerPublish,
    ProducerPublishFinalize,
    // v0.5.x: Add WorkspaceMaterialize (decap) and WorkspaceCapture (encap local capture).
    //
    // decap already runs semantically equivalent Install + Verify stages:
    //   - "install steps" from share spec   ≡  Install stage
    //   - manifest hash / signature / payload_hash checks  ≡  Verify stage
    //
    // WorkspaceMaterialize phases: [Install, Verify]          (no Execute, no Publish)
    // WorkspaceCapture     phases: [Prepare, Verify]          (no Execute, no Publish)
    //
    // Trigger: when §04 sandbox network enforcement is added to Verify, the same
    // enforcement logic must not be duplicated in share/mod.rs. Unifying under
    // HourglassFlow lets error taxonomy (§14), rollback, progress UI, and capability
    // gate be implemented once.
    //
    // See: docs/rfcs/accepted/ATO_CLI_SPEC.md §3.1 内部実装注記
}

impl HourglassFlow {
    pub(crate) const fn phases(self) -> &'static [HourglassPhase] {
        match self {
            Self::ConsumerRun => &[
                HourglassPhase::Install,
                HourglassPhase::Prepare,
                HourglassPhase::Build,
                HourglassPhase::Verify,
                HourglassPhase::DryRun,
                HourglassPhase::Execute,
            ],
            Self::ProducerPublish => &[
                HourglassPhase::Prepare,
                HourglassPhase::Build,
                HourglassPhase::Verify,
                HourglassPhase::Install,
                HourglassPhase::DryRun,
                HourglassPhase::Publish,
            ],
            Self::ProducerPublishFinalize => &[
                HourglassPhase::Prepare,
                HourglassPhase::Build,
                HourglassPhase::Install,
                HourglassPhase::Finalize,
                HourglassPhase::Verify,
                HourglassPhase::DryRun,
                HourglassPhase::Publish,
            ],
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HourglassPhaseSelection {
    pub(crate) flow: HourglassFlow,
    pub(crate) start: HourglassPhase,
    pub(crate) stop: HourglassPhase,
    pub(crate) explicit_filter: bool,
}

impl HourglassPhaseSelection {
    pub(crate) fn runs(self, phase: HourglassPhase) -> bool {
        let phases = self.flow.phases();
        let start_index = phases
            .iter()
            .position(|candidate| *candidate == self.start)
            .unwrap_or_else(|| {
                panic!(
                    "missing start phase {} for flow {:?}",
                    self.start.as_str(),
                    self.flow
                )
            });
        let stop_index = phases
            .iter()
            .position(|candidate| *candidate == self.stop)
            .unwrap_or_else(|| {
                panic!(
                    "missing stop phase {} for flow {:?}",
                    self.stop.as_str(),
                    self.flow
                )
            });
        let phase_index = phases
            .iter()
            .position(|candidate| *candidate == phase)
            .unwrap_or(usize::MAX);

        start_index <= phase_index && phase_index <= stop_index
    }

    #[allow(dead_code)]
    pub(crate) fn runs_prepare(self) -> bool {
        self.runs(HourglassPhase::Prepare)
    }

    #[allow(dead_code)]
    pub(crate) fn runs_build(self) -> bool {
        self.runs(HourglassPhase::Build)
    }

    #[allow(dead_code)]
    pub(crate) fn runs_verify(self) -> bool {
        self.runs(HourglassPhase::Verify)
    }

    pub(crate) fn runs_install(self) -> bool {
        self.runs(HourglassPhase::Install)
    }

    #[allow(dead_code)]
    pub(crate) fn runs_finalize(self) -> bool {
        self.runs(HourglassPhase::Finalize)
    }

    pub(crate) fn runs_dry_run(self) -> bool {
        self.runs(HourglassPhase::DryRun)
    }

    #[allow(dead_code)]
    pub(crate) fn runs_execute(self) -> bool {
        self.runs(HourglassPhase::Execute)
    }

    pub(crate) fn runs_publish(self) -> bool {
        self.runs(HourglassPhase::Publish)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HourglassPhaseResult {
    #[serde(skip)]
    pub(crate) phase: HourglassPhase,
    pub(crate) name: &'static str,
    pub(crate) selected: bool,
    pub(crate) ok: bool,
    pub(crate) status: &'static str,
    pub(crate) elapsed_ms: u64,
    pub(crate) actionable_fix: Option<String>,
    pub(crate) message: String,
    pub(crate) result_kind: Option<String>,
    pub(crate) skipped_reason: Option<String>,
}

pub(crate) fn new_phase_results(
    flow: HourglassFlow,
    selection: HourglassPhaseSelection,
) -> Vec<HourglassPhaseResult> {
    flow.phases()
        .iter()
        .copied()
        .map(|phase| new_phase_result(phase, selection.runs(phase)))
        .collect()
}

pub(crate) fn phase_mut(
    phases: &mut [HourglassPhaseResult],
    boundary: HourglassPhase,
) -> &mut HourglassPhaseResult {
    phases
        .iter_mut()
        .find(|phase| phase.phase == boundary)
        .unwrap_or_else(|| panic!("missing phase result for {}", boundary.as_str()))
}

pub(crate) fn phase_is_ok(phases: &[HourglassPhaseResult], boundary: HourglassPhase) -> bool {
    phases
        .iter()
        .find(|phase| phase.phase == boundary)
        .map(|phase| phase.ok)
        .unwrap_or(false)
}

pub(crate) fn phase_mark_ok(
    phase: &mut HourglassPhaseResult,
    elapsed_ms: u64,
    message: String,
    result_kind: Option<String>,
) {
    phase.ok = true;
    phase.status = "ok";
    phase.elapsed_ms = elapsed_ms;
    phase.actionable_fix = None;
    phase.message = message;
    phase.result_kind = result_kind;
    phase.skipped_reason = None;
}

pub(crate) fn phase_mark_skipped(
    phase: &mut HourglassPhaseResult,
    elapsed_ms: u64,
    message: String,
    skipped_reason: String,
) {
    phase.ok = true;
    phase.status = "skipped";
    phase.elapsed_ms = elapsed_ms;
    phase.actionable_fix = None;
    phase.message = message;
    phase.result_kind = None;
    phase.skipped_reason = Some(skipped_reason);
}

pub(crate) fn phase_mark_failed(
    phase: &mut HourglassPhaseResult,
    elapsed_ms: u64,
    message: String,
    actionable_fix: Option<String>,
) {
    phase.ok = false;
    phase.status = "failed";
    phase.elapsed_ms = elapsed_ms;
    phase.actionable_fix = actionable_fix;
    phase.message = message;
    phase.result_kind = None;
    phase.skipped_reason = None;
}

pub(crate) fn print_phase_line(
    json_output: bool,
    phase: HourglassPhase,
    state: HourglassPhaseState,
    detail: &str,
) {
    if json_output {
        return;
    }
    println!("{}", format_phase_line(phase, state, detail));
}

pub(crate) fn eprint_phase_line(
    json_output: bool,
    phase: HourglassPhase,
    state: HourglassPhaseState,
    detail: &str,
) {
    if json_output {
        return;
    }
    eprintln!("{}", format_phase_line(phase, state, detail));
}

fn format_phase_line(phase: HourglassPhase, state: HourglassPhaseState, detail: &str) -> String {
    format!(
        "PHASE {:<7} {:<4} {}",
        phase.as_str(),
        state.as_str(),
        detail
    )
}

fn new_phase_result(phase: HourglassPhase, selected: bool) -> HourglassPhaseResult {
    HourglassPhaseResult {
        phase,
        name: phase.as_str(),
        selected,
        ok: !selected,
        status: "skipped",
        elapsed_ms: 0,
        actionable_fix: None,
        message: if selected {
            "pending".to_string()
        } else {
            "not selected".to_string()
        },
        result_kind: None,
        skipped_reason: if selected {
            None
        } else {
            Some("not selected".to_string())
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{HourglassFlow, HourglassPhase, HourglassPhaseSelection, HourglassPhaseState};

    #[test]
    fn hourglass_phase_names_match_spec() {
        assert_eq!(HourglassPhase::Install.as_str(), "install");
        assert_eq!(HourglassPhase::Prepare.as_str(), "prepare");
        assert_eq!(HourglassPhase::Build.as_str(), "build");
        assert_eq!(HourglassPhase::Verify.as_str(), "verify");
        assert_eq!(HourglassPhase::DryRun.as_str(), "dry_run");
        assert_eq!(HourglassPhase::Execute.as_str(), "execute");
        assert_eq!(HourglassPhase::Publish.as_str(), "publish");
    }

    #[test]
    fn hourglass_phase_state_names_match_cli_output() {
        assert_eq!(HourglassPhaseState::Run.as_str(), "RUN");
        assert_eq!(HourglassPhaseState::Ok.as_str(), "OK");
        assert_eq!(HourglassPhaseState::Fail.as_str(), "FAIL");
        assert_eq!(HourglassPhaseState::Skip.as_str(), "SKIP");
    }

    #[test]
    fn hourglass_flow_phase_order_matches_consumer_and_producer_specs() {
        assert_eq!(
            HourglassFlow::ConsumerRun.phases(),
            &[
                HourglassPhase::Install,
                HourglassPhase::Prepare,
                HourglassPhase::Build,
                HourglassPhase::Verify,
                HourglassPhase::DryRun,
                HourglassPhase::Execute,
            ]
        );
        assert_eq!(
            HourglassFlow::ProducerPublish.phases(),
            &[
                HourglassPhase::Prepare,
                HourglassPhase::Build,
                HourglassPhase::Verify,
                HourglassPhase::Install,
                HourglassPhase::DryRun,
                HourglassPhase::Publish,
            ]
        );
    }

    #[test]
    fn hourglass_selection_reports_phase_membership() {
        let selection = HourglassPhaseSelection {
            flow: HourglassFlow::ProducerPublish,
            start: HourglassPhase::Verify,
            stop: HourglassPhase::Publish,
            explicit_filter: true,
        };
        assert!(!selection.runs_prepare());
        assert!(!selection.runs_build());
        assert!(selection.runs_verify());
        assert!(selection.runs_install());
        assert!(selection.runs_dry_run());
        assert!(selection.runs_publish());
        assert!(!selection.runs_execute());
    }
}

        /// Lock the ProducerPublishFinalize phase order explicitly.
        /// Any change is a breaking change that requires RFC §3.1 update.
        #[test]
        fn hourglass_flow_producer_publish_finalize_phase_order_is_locked() {
            assert_eq!(
                HourglassFlow::ProducerPublishFinalize.phases(),
                &[
                    HourglassPhase::Prepare,
                    HourglassPhase::Build,
                    HourglassPhase::Install,
                    HourglassPhase::Finalize,
                    HourglassPhase::Verify,
                    HourglassPhase::DryRun,
                    HourglassPhase::Publish,
                ],
                "ProducerPublishFinalize phase order changed — update RFC §3.1"
            );
        }

        /// Security invariant: Verify must always precede Execute in ConsumerRun.
        /// If this fails, hash checks and policy enforcement would run AFTER user
        /// code, which is a critical security regression.
        #[test]
        fn hourglass_verify_precedes_execute_in_consumer_run() {
            let phases = HourglassFlow::ConsumerRun.phases();
            let verify_pos = phases
                .iter()
                .position(|p| *p == HourglassPhase::Verify)
                .expect("ConsumerRun must include Verify");
            let execute_pos = phases
                .iter()
                .position(|p| *p == HourglassPhase::Execute)
                .expect("ConsumerRun must include Execute");
            assert!(
                verify_pos < execute_pos,
                "SECURITY: Verify (pos {}) must come before Execute (pos {}) in ConsumerRun",
                verify_pos,
                execute_pos
            );
        }

        /// All current flow variants must define at least 2 phases.
        /// If this fails after adding a new variant, add a phase-order lock test too.
        #[test]
        fn hourglass_all_flow_variants_have_at_least_two_phases() {
            let variants = [
                HourglassFlow::ConsumerRun,
                HourglassFlow::ProducerPublish,
                HourglassFlow::ProducerPublishFinalize,
            ];
            for variant in variants {
                assert!(
                    variant.phases().len() >= 2,
                    "Flow {:?} must define at least 2 phases",
                    variant
                );
            }
        }

        // v0.5.x: When WorkspaceMaterialize is added to HourglassFlow, add:
        //
        // #[test]
        // #[ignore = "until-hourglass-workspace-materialize-v0.5.x"]
        // fn workspace_materialize_phase_order_is_locked() {
        //     assert_eq!(
        //         HourglassFlow::WorkspaceMaterialize.phases(),
        //         &[HourglassPhase::Install, HourglassPhase::Verify],
        //     );
        // }
