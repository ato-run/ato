# Native GUI / browser 結合の統合証跡

2026-10-24。Runtime [Draft #1543](https://github.com/ato-run/ato/pull/1543)
最終c8db60fd（実行コードc505ea0f）、PWA
[Draft #518](https://github.com/ato-run/ato-pwa/pull/518) b0bf0e1f。
既存#1539/#1540/#1541/#1542とAPI#875/#876/#877、PWA#517の境界を実際に接続した。

通常の共通アプリフレームと固定noVNC1.5.0を、installed Chrome155.0.8059.86から
実LinuxのOwnedNativeRun/HostBoundary UID65534/TigerVNCへ接続し、全工程が成功した。
接続用HTTPS proxyとCoordinator認証は試験専用で、Hosted/staging owner認証の代用ではない。
本物の運営key、OS clipboard、共有Runner credentialは使用していない。

## 成功した観測

- 実640×480フレームを描画。明示Pasteクリック1回、stub clipboard read1回、positive
  ClientCutText1件。実行環境内のXselectionでLatin-1 `636166e9090a`の一致を必須確認。
- 同じRun/Lease/Runner/Session/Surface IDと期限を持つ12項目ACKを受けた後だけ先へ進む。
  permission=falseのpasteはcloseされ、選択内容を変えなかった。
- 実viewport1024×768、同じSurfaceへ再接続。Browser側の2 exchange/2 WebSocket101と
  再描画、Rust側の独立再接続/1024×768×4完全フレームを別の観測として確認。
- permission=falseのresizeは拒否され、geometry不変。元K・D・固定probeを変更せず、
  元Kの全観測、停止後result.json保存、listener/journal/lease root/OCI回収を確認。
- 切断後clipboardは「観測したNone」として別に記録。先行する実貼り付け一致を
  これで代用せず、未観測null、観測false、全体ANDの成否を分ける。
- Browser page errors0、許可以外のpage request0。全静的build bytesに
  Vite HMR clientなし。短命HttpOnly Secure cookieと実Originを確認し、token/headerを
  診断へ出さない。双方のコードはtest-only、product src/K/Receipt変更0。

| 単発観測 | 時間 |
|---|---:|
| helper preparation | 6902 ms |
| lease（browser操作を含む） | 11363 ms |
| Rust実機test全体 | 19.03 s |
| Chrome test case | 8.6 s |
| Playwright起動込み | 9.4 s |
| Root SSH・同期込み | 21.6 s |

これらをstagingアプリ起動性能とは呼ばない。隣のproof JSONにsource/binary/fixture
manifest SHA、相関ID、個別観測を保存した。生ログ・画像・失敗ケースはown .tmpへ保持。
実行後source658入力hashとbinaryが変わらず、独立Docker queryでも当該Runnerの
containers0/networks0を確認した。通常共有Runner6slotには導入していない。

## 失敗と修正も保持

joint1は初期canvasで失敗。旧短時間fixtureが既存10秒Heartbeatを扱わずpanicし、
abort後の回収も既存90秒上限まで遅れた。test-only Coordinatorを既存wireへ合わせた。
旧binaryで10秒超の失敗を再現し、新binaryの12秒待機abortでは1.893秒でfailedと
全回収を確認。本体の10秒/500ms/90秒の制限は変えていない。Root接続スクリプトの
URL path保持、独立SSH master、live log/result同期も修正した。

joint2はBrowserの描画/paste送信/resize/reconnectまで通ったが、切断後のselectionを
paste成功条件として読んでNoneとなり全体失敗。接続中の厳密checkpointを追加した。
貼り付けなしで正しいcheckpoint要求を送る実機では、Noneを正しく拒否し、既に観測した
Ktrue/output_savedtrue、未観測null、cleanuptrueを保持した。成功へ読み替えていない。

PWA側のdirect writeFile(wx)は途中の空/partial fileをpollerへ見せた。旧方式で実FSの
partial JSONを再現し、0600 private pending→fsync→link create-newへ統一した。
並行writer1勝/1拒否・既存file/symlink不変・pending回収・常時完全JSONの回帰を確認。
Node6件、型検査、full build71ページが成功。Runtimeの新guard7件とClippy/fmt成功、
各凍結差分の独立レビューに残るP1/P2なし。広範な旧PWAコピー期待値1失敗は別記録のまま。

## 共有stagingと次の導入境界

API/PWA code-onlyのphase16配備と実5資産確認は各repoのstage16 opsを参照。
今回の2 PRはtest-onlyでcustomer bytes差分0のため追加配備していない。feature、
migration、key、実job/Run/Try、承認/掲載、GPU、本番には変更なし。

readonly systemdでは旧staging共有slot1..6が同じRunner IDで稼働し、slot1 Userは
既存のekohsukeだった。同じIDに異なる能力のbinaryを混在させない。導入には以下が必要。

1. 統合した通常Runner binaryのsource/hash/起動条件を確定する（現在prepare-onlyで検証中）。
2. 既存Static/Process/OCIのUID・保存領域と6slotを保つ。Native HostBoundaryを別の
   dedicated identityへ置く場合、通常のEnrollment/Lease/Heartbeat/Recoveryを再利用し、
   既存credentialのコピーや独自認証を使わない。共有環境の切替は両chatと調整する。
3. 対象ownerのstaging認証を確定してから、実GitHub Source job→選択D→Hosted lease→
   元K receipt永続化と再接続/保存を同じattemptで確認する。既存の手動ログイン質問は
   未回答。canonical lifecycle skillの「manual login only」「staging-only Chrome profile」
   条件を保持し、cookie/token取得やAdminのowner代用で迂回しない。

運営Brokerはpolicy0で無効、GPUの実Provider試験は未実施。保守的予算gateとVast設定の
未解決条件を解除していない。依頼全体の完了やHosted全種類の受入完了は主張しない。
