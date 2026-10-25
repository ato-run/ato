# Formation attempt journalのWindows保存境界

Status: Draft。ローカルattempt journalのplatform実装を記述する。Record schema、Source/K/D、Runtimeの実行許可、Coordinatorの台帳契約は変更しない。

## 保存と実行の順序

既存の予約 `NotStarted`、実行前の `Started`、停止・検証結果確定後の `Finished` を同じJSON schemaで保存する。開始recordの保存が成功するまで実行しない。履歴を読めない場合や保存失敗は `HistoryUnavailable` / `NotDurable` とし、開始済みで終了不明のrecordを「未実行」へ読み替えない。

共通の書込み処理は、同じdirectory内のtemporaryへ完全なJSONを書き、`File::sync_all` が成功してから公開する。Unixは既存のrenameとdirectory syncを維持する。

Windowsの公開は `MoveFileExW` に `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH` を指定する。temporaryとdestinationは同じparentに限り、copy fallbackとreboot後の移動は許可しない。Windows APIの失敗はOS error付きで呼出し元へ返す。directoryを通常の `File::open` で開いて失敗する処理を、エラー無視で成功へ変換しない。

Microsoftの[MoveFileExW仕様](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)は、WRITE_THROUGHについて移動がdiskへ反映されるまで戻らないと記述する。一方、明示されたflush保証の説明はcopy/deleteの移動を対象とする。この実装は同期済fileとsame-directoryの同期付き移動という公開API契約を利用する。任意filesystem・storage hardware・電源断に対する実験的証明を主張しない。

既存のlocal-executionの `atomic_write` はrenameのみ、objectsのhelperはfile syncとrenameのみであり、このattempt保存境界の同期付き公開を代替しない。別の汎用storage実装は追加せず、既存journalの公開処理にplatform実装を置く。

## request lockと互換性

予約・history照合・実行・finishを覆う既存delivery permitのexclusive lockをWindowsでも取得する。[Rustの `File::lock`](https://doc.rust-lang.org/std/fs/struct.File.html#method.lock) をread/writeで開いた既存 `.lock` に適用し、permitのdropまで保持する。Windowsでlockが未取得のまま共有temporaryを更新する挙動を許可しない。Unixの既存 `flock` は維持する。

保存先・既存record bytes・identityの解釈は維持し、過去recordを再生成しない。publicationに失敗した場合にtemporaryを成功recordとして読まず、既存の保守的なhistory照合を通す。

Windows native CIでは予約・開始・終了の置換/reopen、readonly destinationでのpublication拒否と旧record保持、同一requestのlock contention、UNKNOWN/redeliveryの既存fixtureを検証する。cross targetのcheck/clippyをWindows native実行の代用にはしない。
