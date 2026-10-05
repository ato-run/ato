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
