# Windows attempt journalの限定修正

測定ID `formation-windows-attempt-journal-local-20261003-01`。修正codeを `478ab4857dcaad91ddb090c3b30b4c38c87b675e` へ保存した。ローカル検証はcommit前の同じcode bytesを対象とし、後の文書commitへ測定pinを付け替えない。Windows native受入はこのローカル測定に含まれない。

## 失敗と差分の照合

Actions run [37109321016](https://github.com/ato-run/ato/actions/runs/37109321016)、CI head `5798e23d28aaebd4fabd6d44cecef51ab869c717` のWindows portable testsは7 PASS / 8 FAIL。失敗は実行前のattempt記録にあるdirectory open/syncで `Access is denied. (os error 5)` となった。個別Win32 callのtraceは取得していない。

`lib/runtime-attempt/src/journal.rs` のblobはbase `31a6d6b4` とhead `5798e23d` で同じ `7df4be70e8d82b006f5a40ff466b1709e64bfb94`。このplatform境界のコードは今回CI差分以前から存在する。exact baseのWindows実行比較を行ったと報告しない。

既存実装はsync済fileのrename後にdirectoryを通常の `File::open` で開いてsyncしていた。Windowsの[CreateFile仕様](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)ではdirectory handleの取得にbackup-semantics flagが必要で、[FlushFileBuffers仕様](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers)はwrite accessを要求する。コードと失敗ログから、このUnix向けdirectory open/syncを共通使用したplatform境界が原因と判断した。

Windowsでは同期済temporaryを同じparentの保存先へ `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)` で公開し、失敗は引き続き開始拒否へ返す。Unixのrenameとdirectory syncは維持した。Windowsでも既存delivery permitのrequest lockを `File::lock` で取得し、共有temporaryへの同時更新を防ぐ。過去record・schema・Source/K/D・Coordinator台帳は変更しない。[Draftの保存境界](../rfcs/draft/FORMATION_ATTEMPT_JOURNAL_WINDOWS.md)にAPI保証の範囲を記録した。

## 新しいローカル検証

| 対象 | 結果 |
|---|---|
| macOS arm64 / Rust 1.96.0、journal unit tests | 7 PASS。予約→開始→終了の置換/reopen、Unicode root、UNKNOWN/redelivery、保存失敗、lock contentionを含む。 |
| Windows GNU cross target、Runtime all-target strict clippy | PASS。Windows専用readonly destinationのpublication拒否・旧reservation保持テストもcompileした。実Windows上での実行とは区別する。 |
| 変更したjournalのrustfmt、diff check | PASS。 |
| workspace fmt check | 既存の未変更reasoning.rsにformat差分がありFAIL。この限定修正では変更していない。 |

明示したtargetは既存 `.tmp/formation-agent-session/ato/.tmp/ci/windows-target`。Windows向けRuntime・Formation・HTTPのpackage artifactを明示invalidate（196 files / 約457.8 MiB）した後、verbose compile出力でこの新worktreeのSourceとtargetを確認した。ローカルでは `CARGO_INCREMENTAL=0`、一時rootは当該worktreeの `.tmp/` を使用した。

既存CLI CIへWindows限定のjournalテストstepを追加した。Windows nativeでは8件となり、readonly destinationでの置換失敗と旧record保持も実行する。Windows portable test全体とこの新stepの結果は、修正後Actionsで別途確認する。CI既存ログはstable Rust 1.99.0 / MSVCであり、ローカル1.96.0 / GNU cross targetと同じ環境として扱わない。

電源断試験、任意filesystemの耐久性、実Codex/Claude Code、OSSの機能・保存・再起動は未測定。推論API call、実受入Runは0。配備・remote migration・flag変更・既存測定の修正もない。全ログのhashとcode blobを [機械記録](evidence/formation-windows-attempt-journal-local-20261003.json) に保存した。
