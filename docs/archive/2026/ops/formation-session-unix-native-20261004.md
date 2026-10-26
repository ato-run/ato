# Formation Session / Native Skill follow-up — 2026-10-04

Session #1479にUnix socket relayを追加した。Requesterのprivate接続とProducerの期限付きdescriptorを分離し、両agentは同じ4つのMCP操作を使う。予算、入力digest、応答の原子的保存、固定K、Runtime実行、receiptのauthorityは既存経路が所有する。

## 実装と測定pin

| 項目 | 実行pin / 結果 |
|---|---|
| Unix relay | integration `da9ca69b1613ca47581e819bfb0528466e1c42e3`。CLI lib 53件、MCP mode 1件、all-target strict clippy、format/build PASS。 |
| Native package helper | `b88f25866fca74ed4d667aed2dd60761e1292985`。Skill 26件 PASS。Code Mode hostのoptional固定とsystem ICU/bootstrapの限定許可。 |
| 同UIDのOS負例 | 同pinのC fixture。14件 PASS。owner値、credential、grant、ticket、DB、Source、過去報告のread/write、symlink/fork、host直接実行、別socket/TCPを拒否。選択Unix socketへのlive接続だけ成功。Native推論ではない。 |
| Unix testのCI portability | `acf79cf81ae5d7d1e915cdec8e73a9c7dc47e6be`。TMPDIR未設定で新規4件 PASS、Skill 26件 PASS。Session側の対応commit `5ecec3dbdd26e230adcc86d66dec61493dde1742`。 |
| Windows installer | Session `8bd2525046ed925eff862198d22fcf398f0ab8e3`、integration `693bb45be`。MCP binaryをMSI対象へ追加。 |

固定MCP binaryは`da9ca69b`からbuildしたもの、SHA-256 `b3b9a651581f997b649d6231e4b9256118019948762cdfa86fcc9b32816138a2`。後続のhelper/docs/test/installer変更を、このbinaryのsource pinへ付け替えない。詳細hashは[evidence](evidence/formation-session-unix-native-20261004.json)を参照。

## Native loader / tool fixture

global Codex 0.46.0は保持した。受入用の公式Codex 0.160.0とCode Mode hostのrelease asset digestを照合し、別directoryへ配置した。Claude Codeは2.1.288。

実Codexの`skills/list`は共通packageを検出した。実CLIをcontrolled providerへ接続したfixtureでは、CodexとClaude Codeの両方でSkill本文の明示展開を確認した。Claudeの`--restricted`と空のsetting sourcesはproject Skillも除外するため、公開projectだけを設定源にして`--tools "" --strict-mcp-config`を用いる構成へ補助文書を修正した。

Codexのtool定義は通常の`tools` fieldだけでなく`additional_tools`とCode Modeのnested toolに現れる。`agents.enabled = false`を含む設定でsubagentを除き、公式model metadataの識別子とpromptを保ってshell/patchの能力だけを制限したcatalogで、shell・patch・画像・subagentのtoolを除いた。実Code Mode fixtureではNode require/process/fetch、Node filesystem import、shell/patch、追加MCP toolが利用できないことを7項目で確認した。これはcontrolled応答から実Native toolを実行した負例であり、実LLM callではない。

Claudeは既存accountのOS credential storeを使う認証ホストのstatusでlogged-inを確認した。plaintext credential fileは許可していない。認証ホスト用policyはprepared OS fixtureとは別pin/profileであり、同じ隔離が成立したとは報告しない。Codexの孤立したkeyringは限定recipient ACLの妥当性を確認したが、Native lookupが待機したため認証成立は未確認。globalの通常ログインが失効したという結果ではない。

このfollow-upの実provider推論、Source Search、Runtime attempt、fresh receiptは0。Native actor別unique app成功数も0であり、旧Codex Source探索の成功数を変更しない。独立有料API枠は消費していない。

## CI

Session `23ae6ba88005df2190308b634504578b1fa26e12`ではUbuntuとnative Windows/MSVCがPASS、macOSで追加4テストのTMPDIR条件がFAILした。これは新規regressionとして修正した。

`5ecec3dbdd26e230adcc86d66dec61493dde1742`では追加4テストを含むCLI lib 53件がmacOSでPASS。push run [37134945155](https://github.com/ato-run/ato/actions/runs/37134945155)は3OSともPASSだった。同じheadのPR runでは既存`compiler_error_is_a_valid_portable_handoff_point`がrepository JSON EOFでFAILした。該当test、repository、supervisorのblobはCI base `2043a9fdd`と同一だが、このEOFのexact base実行再現と原因修正は未確認。並行runの成功を原因解消へ読み替えない。

Release planはMCP binary追加後のMSI定義が古いためFAILし、installer定義を修正した。Session `8bd2525046ed925eff862198d22fcf398f0ab8e3`のPR run [37135367549](https://github.com/ato-run/ato/actions/runs/37135367549)とpush run [37135364616](https://github.com/ato-run/ato/actions/runs/37135364616)はUbuntu/macOS/native WindowsともPASS。architectureと[Release plan](https://github.com/ato-run/ato/actions/runs/37135367540)もPASS。release/uploadは実行していない。

## 実受入の継続条件

今回のユーザー指示によって新規4セルの実受入は承認範囲に入った。旧20261003計画の未承認/未実行表記は当時の記録として保持し、後続計画へ許可と未成立gateを記録する。認証・tool/OS境界・provider通信・最終binary/API pinの確認を、実Source受入の前に行う。

Linuxのfree spaceが20 GiB未満だったため、対象作業の再生成可能な`target/debug/incremental`だけを削除した。実行中cargo/rustcなし、同UID・canonical pathを確認した。34,863 file / 6,101,113,484 bytesのcacheを対象とし、freeは18,638,991,360から24,191,029,248 bytesへ増えた。Source、固定binary、artifact、state、receipt、journalは保持した。Sourceを実行する直前にもdiskを再確認する。

PWA入力/権限変更後resume、新Kutt artifactの機能・保存・新Run復元、独立API4計画は未実行。旧WBO UNKNOWN、過去の測定pin/receipt/台帳は保持した。配備、remote migration、shared flag変更、100件再測定、課金付き自動調達はこのfollow-upで実施していない。

## 実Source受入と独立API — 2026-10-04続報

後続helperは待機中attemptと保存済み応答を先に照合し、新しい入力がない間はNative推論を始めない。Native SDKのpipeでは複数frameを同じreadで受けた場合も全frameを処理する。現在のSkill/launcher検証は35件PASS、Owner専用のtool出力漏出検査は2件PASSである。

実Codex 0.160.0 / gpt-6.1-solから共通Skillを明示呼び出した新規SVGOMG Searchは、既知Dなしの初回lifecycle失敗後にDを修復し、3 exchange・2 D round・2 Runtime attemptで同じ固定Kのfresh PASSに到達した。2 attemptともRuntimeのaccepted ACK保存とdelivery終了を確認し、PASS候補の停止とcleanup、Search予約ゼロを確認した。状態は k_reached_awaiting_assessment であり、通常Run許可は与えていない。[保存証拠](evidence/formation-native-codex-source-20261004.json)。Nativeの内部call/token/費用は不明のままである。

この成功に先行した3つのCodex SearchはRuntimeのsocket長または隔離内から読めない固定binaryのため終端した。1件では応答保存後のNative client切断から、同じSearch・元deadline・消費枠を保って次のexchangeへ再接続した。保存済みr1の再推論・再実行は行っていない。infraの解消ではOwner領域の権限を緩めず、検証済みbinaryの同一bytesをRuntime管理領域へ固定した。

実Claude Code 2.1.288は4つのMCP toolと共通Skillを読み込み、実model claude-opus-5-5 で接続したが、既存accountのquotaで推論開始前に拒否された。このSearchはOwnerが実行前に取消し、停止・input cleanupを確認した。quota解除後の同一Source/K・初期情報・上限の別Searchは未実行であり、Claude Code Source対応の受入完了とは報告しない。

Session 8bd2525 のnative Windows/MSVCを含む3OS CIはPASS。[具体的なWindows環境・test結果](evidence/formation-native-platform-ci-20261004.json)を補った。以前のLinux WebSocket/macOS worker起動失敗のexact base原因再現は引き続き未確認であり、今回のgreenから過去の原因解消を推定しない。

ユーザー指定のrotation credentialはDeepSeekのbalance APIで利用可能を確認し、値はメモリ内だけで取り扱った。既存18 call・小規模残り23 call・未精算予約ゼロを照合した後、独立SVGOMG API Searchを実行した。成功Dをseedにせず、3 call・2 round・2 attemptで権限不足とlifecycle未宣言の証拠を残して枠切れとなった。これはCodexの成功とは別の失敗であり、同じ終端Searchの枠やdeadlineを復活させない。全体21/41 call、小規模4/24 call、推定消費$0.097718、残予算$0.467188、未精算予約ゼロである。推定は予約用peak価格に基づき、providerの実請求明細ではない。

PWA入力・権限変更後resume、新Kutt artifactの機能・保存・新Run復元は別gateとして検証中である。共有環境への配備、remote migration、shared flag変更、100件再測定、旧WBO UNKNOWN再実行、課金付き自動調達は実施していない。
