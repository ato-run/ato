import React, { useState, useEffect, useCallback } from "react";

import { ConsentScreen } from "./screens/ConsentScreen.jsx";
import { BootScreen } from "./screens/BootScreen.jsx";
import { GithubRunScreen } from "./screens/GithubRunScreen.jsx";
import { CandidatesScreen } from "./screens/CandidatesScreen.jsx";
import { CandidateDetailScreen } from "./screens/CandidateDetailScreen.jsx";
import { NoCandidatesScreen } from "./screens/NoCandidatesScreen.jsx";
import { CreateTomlScreen } from "./screens/CreateTomlScreen.jsx";

/**
 * Root app — screen state machine for the ato-launch SPA.
 *
 * Initial screen is determined by `window.__ATO_LAUNCH_SCREEN`:
 *   'consent'     → ConsentScreen  (default for normal capsule launch)
 *   'boot'        → BootScreen
 *   'github_run'  → GithubRunScreen (GitHub source execution flow)
 *
 * Screen transitions are driven by IPC commands posted from React back to
 * Rust via `window.ipc.postMessage`, and by direct React state updates for
 * in-process transitions (e.g. candidates → candidate_detail).
 */
export default function App() {
  const initialScreen = window.__ATO_LAUNCH_SCREEN ?? "consent";

  const [screen, setScreen] = useState(initialScreen);

  // Shared GitHub flow state
  const [githubState, setGithubState] = useState({
    repo: "",
    candidates: [],
    selectedIndex: null,
    customToml: null,
  });

  const navigate = useCallback((nextScreen, updates = {}) => {
    if (Object.keys(updates).length > 0) {
      setGithubState((prev) => ({ ...prev, ...updates }));
    }
    setScreen(nextScreen);
  }, []);

  // Listen for Rust-initiated screen navigations via a custom event
  useEffect(() => {
    const handler = (e) => {
      const { screen: next, ...rest } = e.detail ?? {};
      if (next) navigate(next, rest);
    };
    window.addEventListener("ato:navigate", handler);
    return () => window.removeEventListener("ato:navigate", handler);
  }, [navigate]);

  switch (screen) {
    case "consent":
      return <ConsentScreen />;

    case "boot":
      return <BootScreen />;

    case "github_run":
      return (
        <GithubRunScreen
          onCandidatesFound={(candidates, repo) =>
            navigate(
              candidates.length > 0 ? "candidates" : "no_candidates",
              { candidates, repo }
            )
          }
        />
      );

    case "candidates":
      return (
        <CandidatesScreen
          candidates={githubState.candidates}
          repo={githubState.repo}
          onSelect={(index) => navigate("candidate_detail", { selectedIndex: index })}
          onCreateOwn={() => navigate("create_toml", { customToml: null })}
        />
      );

    case "candidate_detail":
      return (
        <CandidateDetailScreen
          candidate={githubState.candidates[githubState.selectedIndex]}
          onBack={() => navigate("candidates")}
          onProceed={() => {}}
        />
      );

    case "no_candidates":
      return (
        <NoCandidatesScreen
          repo={githubState.repo}
          onCliInference={() => navigate("create_toml", { customToml: "__cli_inference__" })}
          onCreateManually={() => navigate("create_toml", { customToml: "" })}
          onReviewUrl={() => navigate("github_run")}
        />
      );

    case "create_toml":
      return (
        <CreateTomlScreen
          initialContent={githubState.customToml}
          repo={githubState.repo}
          onSave={(content, meta = {}) => {
            const syntheticCandidate = {
              title: "カスタム capsule.toml",
              author: "（自分で作成）",
              status: "unverified",
              source: "local_draft",
              manifest_source: meta.manifest_source || "user_edited",
              description: "このセッションで作成した capsule.toml",
              toml: content,
              repo: githubState.repo,
            };
            navigate("candidate_detail", {
              candidates: [syntheticCandidate],
              selectedIndex: 0,
              customToml: content,
            });
          }}
          onCancel={() => {
            // Return to wherever we came from
            const hasExistingCandidates = githubState.candidates.length > 0;
            navigate(hasExistingCandidates ? "candidates" : "no_candidates");
          }}
        />
      );

    default:
      return <ConsentScreen />;
  }
}
