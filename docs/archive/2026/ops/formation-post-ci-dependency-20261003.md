# CI依存を含むreview unitのnative確認

測定ID `formation-post-ci-dependency-20261003-01`。CI #1478をlocal mergeした
Sessionとstateの自然なGitHub Actionsを確認した。manual rerunは0。
後続の文書commitへ成功pinを付け替えない。

| 単位 | 実行pin | CI | 結果 |
|---|---|---|---|
| Session | `f8fe576f` | [37114199921](https://github.com/ato-run/ato/actions/runs/37114199921) | Ubuntu/macOS/WindowsすべてPASS。Windows journal8件とserial test/clippyもPASS。 |
| state | `574a556a` | [37114230773](https://github.com/ato-run/ato/actions/runs/37114230773) | Ubuntu/macOS/WindowsすべてPASS。Windows journal8件とserial test/clippyもPASS。 |
| API state/CI依存 | `e3b6aad2` | [37113949799](https://github.com/ato-run/ato-api/actions/runs/37113949799) | typecheck・Instance/browser/Chromium/WebKit PASS。Activity/CORS13失敗、full-serial skipped。 |

APIの13失敗名は保存済みexact base `02a9e58b` / head `9b483d4a`の両方と一致し、
新しい失敗名は0。別main `9e003190`との比較は別pinのまま残した。
現在のexact baseで再実行した結果とは扱わない。
[機械記録](evidence/formation-post-ci-dependency-20261003.json)に各run/step、
log/metadata hash、保存した失敗名と比較先を残した。

これはnative CLI CIであり、実Codex/Claude Codeの探索受入ではない。
元macOS worker失敗の原因確定、Producer通信境界、PWA実入力/resume、
今回探索artifactの機能/保存/新Run復元、完全なprivate値非露出の証明へ
読み替えない。PRマージ、配備、remote migration、flag変更、実Search作成は0。
