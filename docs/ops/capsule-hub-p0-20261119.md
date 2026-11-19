# OSS調査のP0をstaging Capsule Hubへ登録した記録

2026年11月19日 12:21 JST 時点で、依頼時の301件のうちP0全58件（既存調査分34件、新規調査分24件）を `koh0920` 名前空間に公開した。Hub全体では既存7件を含めて65件になった。台帳にはアプリごとの詳細ページ、実行方式、最新バンドルのSHA-256、ContractRef、DerivationRef、起動条件を記録した。[staging Capsule Hub](https://stg-app.ato.run/capsules) · [全58件の台帳](evidence/capsule-hub-p0-20261119/catalog.csv)

## 公開・実行の確認結果

全58件について、ページ送りを含む一覧、詳細、タグ解決、配信ファイルを取得した。表示名と上流リポジトリが調査記録に一致し、配信サイズ・SHA-256・バンドルのバイト列も手元の成果物と一致した。

| 最新版の確認範囲 | 件数 | 確認内容 |
| --- | ---: | --- |
| CLIのContractとブラウザ操作が成功 | 1 | Excalidraw。最新digestを指定したCLI RunでHTTP応答の本文digestまで一致した。Hub配信物から取り出したサーバーとUIで、矩形の描画・保存・再読み込み・JSON書き出しを確認した。 |
| CLI実行で起動失敗を再現 | 2 | RustDesk、LibreOffice。上流DMGのSHA-256照合後、アプリ内のリンクを展開する段階で失敗した。 |
| イメージ構成とCLI仕様から起動制約を特定 | 4 | GitLab CE、ONLYOFFICE Docs、Formbricks、Plane。現在のバンドルに、公式イメージが要求する全マウントを割り当てられない。 |
| 実機起動未確認 | 51 | 必要なデータ、モデル、認証、外部接続、試用版の機能と保存範囲を各アプリのREADMEと台帳に記載した。 |

実行方式はLinux/amd64のOCIが48件、Apple Silicon向けネイティブGUIが9件、Python 3.12のローカルプロセスが1件。OCIは手元のDockerデーモンがCLIの5秒の照会に応答せず、確認した所有Runtimeは3台ともオフラインだった。GrafanaはHub参照の解決後、同じ理由でRun開始前に停止した。Hubの表示は「確認済み」1件、「実験的」57件だった。[観測記録とreceipt抜粋](evidence/capsule-hub-p0-20261119/observations.json)

調査CSVが作業中にも拡張され、同じIDが別のアプリを指す変更があった。PostgreSQL・Pandoc・WordPressの表示名と出典を訂正した。最終照合には依頼時のP0と、301件時点のアプリID・上流リポジトリの対応を使った。全58件で調査ID・上流リポジトリ・Hub参照がそれぞれ一意である。

## CLI・Hubの不具合と使い勝手

優先度は今回の利用者への影響による。製品側で直した後に何を確認するかを右欄に示す。

| 優先度・対象 | 現象と影響 | 改善と受け入れ条件 |
| --- | --- | --- |
| 高・Hub検証表示 | Excalidrawの新しいdigestを公開した直後、その版のRunは0回だったが、APIは旧版の成功1回と `verified` を返した。その後、新版の成功を別途確認した。詳細画面から現在の版が動いたかを判断できない。 | 選択中のdigestに一致する成功の有無を表示する。新版の最初の成功前は未確認、成功後は確認済みになり、旧版の履歴も参照できること。 |
| 高・Hub検索 | `/capsules?q=interop-static` を直接開くと「検索に一致するカプセルはありません。」と表示された。同じ画面で「さらにロードする」を押すと既存の `koh0920/interop-static` が現れた。初期検索の範囲が最初の50件に限られる。 | 未読ページも検索対象に含める。51件目以降の名前から追加読み込みなしで詳細に移動できること。 |
| 高・CLIのmacOSアプリ展開 | RustDeskの `window_size.framework/window_size` とLibreOfficeの `Contents/MacOS/urelibs` で `native_install_materialization_failed` を再現。RustDeskのリンク先はバンドル内の `Versions/Current/window_size`。ウィンドウ起動に到達しない。 | バンドル内で解決するframeworkリンクを扱えるようにする。外部・循環リンクは拒否し、展開後の署名検証を維持した上で両実物を起動できること。 |
| 高・CLIのOCI事前診断 | 上記4件はpackと公開に成功する。GitLabは3パス、ONLYOFFICEは4パスを要求する一方、現在のプロファイルが宣言できる永続領域は1個。未割当パスが一覧に出ない。 | イメージ設定を参照できる時点で未割当パスを列挙する。未検査の状態も表示し、必要な永続領域・一時領域を明示して起動できること。 |
| 中・CLIのバックグラウンドworker | TwentyとChatwootのworkerはポート省略で `missing field ports`、空配列で `every service serves at least one Port` となりpackが失敗する。公開した試用版ではメール配信・一括処理を利用できない。 | 待受ポートを必要としないworkerの起動・生存確認を表現する。Web・DB・キュー・workerを含む例で確かめる。 |
| 中・CLIの容量エラー | 一般公開者として10,797,471バイトのExcalidrawを送ると `hub-bundle-too-large`。エラーには上限も実サイズも表示されない。Clineは上流v3.50.0のVSIX拡張ファイル一式を圧縮して7,784,270バイトで公開した。 | 一般公開枠の8 MiB、実サイズ、超過分を表示する。容量ポリシーを参照できる場合は転送前に案内する。 |
| 中・実行前の必要条件 | Excalidrawの必要条件は「プラットフォーム —」、実行方法は「local_process」。必要なPython 3.12はREADMEにある。Grafanaのコピー用コマンドに必須の `admin_password` bindingが入らない。 | 実行方法の近くにランタイムのバージョン、必須bindingの名前、永続・一時データを表示する。コピー用コマンドに秘密値を保存せず入力方法を示す。 |
| 中・アプリの操作確認 | 初版ExcalidrawはHTTP本文が一致しHubに成功Runが記録されたが、シーン読み込み先がなく描画面が開かなかった。 | HTTPの確認対象と主要操作の実機確認を区別して表示する。ホワイトボードでは描画・保存・再読み込み・書き出しを操作確認として残す。 |
| 低・公開者とカテゴリ | Excalidrawの「作者」は登録者の `koh0920`。上流はソース欄にある。今回の58件はすべて `primary_category` が空で、カテゴリからは発見できない。`ato hub publish --help` にカテゴリ指定はない。 | 「公開者」「移植担当」「上流プロジェクト」を区別する。公開時に既存カテゴリを選択でき、一覧のフィルターに反映されること。 |
| 低・CLIヘルプ | `ato hub --help` は冒頭で暗号化共有を説明する。同じコマンド群の `publish` はバンドルとメタデータを公開する。 | 最初の画面で `push/pull` の共有リンクと `publish` の公開範囲を読めること。 |

検索の実画面は[追加読み込み前](evidence/capsule-hub-p0-20261119/search-before.png)と[追加読み込み後](evidence/capsule-hub-p0-20261119/search-after.png)に保存した。[モバイルの詳細画面](evidence/capsule-hub-p0-20261119/ui-mobile.png)

## 登録時に選んだ試用条件

- ExcalidrawはPython標準ライブラリで同梱アセットを配信する。シーンの処理には上流のハンドラーを使い、「Save in this browser」はそのアドレスのブラウザ内に保存する。別のRunへ持ち出すには「Export scene」を使う。公開バンドルは3,621,881バイト。書き出したJSONには幅235の矩形1個が含まれていた。[描画の証跡](evidence/capsule-hub-p0-20261119/drawing.png)
- Immichのアップロード画像、Moodleの教材添付、OdooのDBは試用中の一時データになる。継続利用を判断する際は各行の保存範囲を参照する。
- Cline・Gemini CLIなどの外部モデル連携、OpenHandsのエージェント実行、RustDeskの遠隔接続、ComfyUIとPaddleOCRのモデルを使った処理は未確認。macOS版は上流DMGのURL・サイズ・SHA-256を固定した。QGISとOpenHandsの実行ファイル名は上流ビルド設定から取得した。
- CLI 0.9.5でstaging APIを明示してコンテンツを公開した。アプリ、API、PWA、CLIの製品コードの配備・migration・feature flag変更は行っていない。

CLIの根拠は[OCIのVOLUME検査](../../extensions/adapters/oci/src/lib.rs)、[サービスとstateの検証](../../lib/formation/src/capsule_toml_v2.rs)、[macOS DMGの展開処理](../../apps/portable-application/src/native_desktop/dmg.rs)を参照。実行したCLIのコミットは `8ee783ba3`。staging系列のAPIは `8fee5cbfa` の `src/routes/hub_capsules.ts`、PWAは `20edcd247` の `src/components/capsule-hub/CapsuleHubPage.tsx` で、検証回数の集計と検索対象を確認した。

## 同日の起動・主要操作の追試

対象は初回公開時に凍結した同じ58件。stgの `capsules/koh0920/` から配信digestとDを指定してMac上で実行した。Docker Desktop 29.1.3のデーモンを復旧し、Apple Silicon上でlinux/amd64の実行も確認した。アプリごとの配信digest、D、試行結果、主要操作は[追試一覧](evidence/capsule-hub-p0-20261119/runtime.csv)、観測結果と実行方式は[receipt抜粋](evidence/capsule-hub-p0-20261119/runtime-receipts.json)に記録した。各Runに独立した作業領域を割り当てた。

| アプリ | 実際に確認した操作 |
|---|---|
| Excalidraw | 矩形を描いてブラウザーに保存し、再読み込み後に書き出したJSONから矩形を復元できた。 |
| WordPress | stgの配信版で初期化後、管理者でログインし、下書きを作成・再取得して本文と下書き状態を照合した。 |
| Trilium Notes | 空のノートブックに子ノートを作成し、日本語のタイトルと本文を保存した。ページの再読み込み後、ツリーと編集画面に同じ内容が表示された。 |
| Mermaid | Python 3.12のDで二つのラベルを描画し、SVGをダウンロードして両方のラベルを照合した。 |
| Memos | 一時環境の利用者でサインインし、非公開メモを作成してAPIから同じ本文を読み出した。 |
| Pandoc | stgのOCI経路へMarkdownを送り、見出しを含むHTMLを取得した。 |
| Beszel | 初回管理者を作成し、監視対象を登録して名前を再取得した。エージェントからの計測値受信は未確認。 |

Mermaidの現行配信は `sha256:b6d9d279c723bc68c21a7bf4b0bafa56894d012ce50964a462c46c9c9e3b4e27`。契約と従来のOCI Dを保ったまま `sha256:d4e7d04a43e19ab1a1694c6f907aab73040193edad81128d499ed2e2850985a6` を追加した。利用者は次のようにDocker不要のDを選択できる。

```sh
ato run --api-url https://staging.api.ato.run --derivation sha256:d4e7d04a43e19ab1a1694c6f907aab73040193edad81128d499ed2e2850985a6 capsules/koh0920/mermaid
```

### 起動を妨げた不具合と操作上の問題

| 箇所 | 実測 | 対応・改善点 |
|---|---|---|
| CLIのOCI経路選択 | Docker Desktopでは実行可能と表示された単一コンテナを、Run開始時にrelay未対応として拒否した。 | 単一OCIにもRun専用の内部ネットワークと既存relayを割り当てた。MemosとPandocで配信済みのDをそのまま選択し、実操作とreceiptを確認した。 |
| OCIグループの起動失敗 | PostgreSQLの初期化サーバーは `/tmp` で待ち受け、上流エントリーポイントの `psql` は `/run/postgresql` を探して終了コード2になった。 | 初期化クライアントとサーバーのソケットを合わせる。エミュレーション中は初回DB構築が従来の60秒を超えた。 |
| WordPressの初期化 | 配信初版は全コアファイルのコピーで待受が間に合わず、コピーを短縮した版は未設定サイトからのHTTP 302でContractに失敗した。 | イメージ内のライブラリを参照し、既存サイトを検出して初回だけ管理者を作成する構成に更新した。stg版の配信SHA-256は `921881c75c511725a156b87fcb34b3c739226be280500d6f79fa495f4d5503cd`。初期パスワードと署名用の鍵を別々のbindingで渡す。 |
| SiYuanの起動完了判定 | 純粋なDで宣言した `bootProgress` への観測だけで追加の実行権限を求められた。 | 選択したDのポートへの開始観測と、ネットワーク・権限付与を分けた。配布イメージの初期化条件は別途確認する。 |
| macOSのDMG | 署名済みアプリのFramework内にある相対リンクを展開時に一律拒否した。 | アプリ内で解決できるリンクを復元し、対象の表記もツリーダイジェストに含めた。RustDeskは展開後に終了コード255、LibreOfficeはウインドウ観測前に終了コード0となった。 |
| AppFlowyの表示 | macOSのウインドウ観測は成功したが初期画面は黒く、文書を編集できなかった。初回はFlutterが実ユーザーのLibraryへの書き込みを拒否された。 | FoundationのホームをRunの保存領域に指定するとプライベートDBの初期化まで進んだ。再起動後も黒画面を再現し、ログにセッション未作成が記録された。 |
| 大きな配信物・取得状況 | Pandoc 3.14の公式WebAssemblyでローカル変換できた。OCIとの併載版は約22.7 MBとなり、stgの1件8 MiBの上限で拒否された。OCI取得失敗時に原因が空のCLIメッセージも確認した。 | publish前に実サイズと上限を表示する。取得中の対象と進捗、タイムアウトと実プロセスの終了状態を区別して表示する。 |
