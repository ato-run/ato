# ato-run/ato Lean バグチェック報告（2026-11-17）

## 主要機能の棚卸し（第1回、対象 c4f29fe5f）

優先度は影響範囲と調査順を示す。下記は機能ごとの網羅保証ではなく、現在確認した境界を明記するための一覧。API / PWA / Desktop は本 repo 外または別ビルド境界であり、今回は未検査。
この表は**第1回時点**の記録で、その後の更新は下記「第2回 棚卸し」の表を参照する。

| 優先 | 機能 | 今回の確認と状態 | 次に調べる範囲 |
|---|---|---|---|
| 高 | Run の記録とローカル保存 (`lib/objects`, `lib/local-execution`) | **新規の不整合を1件再現**。下記 B1。既存 object/repository/bundle 単体63件は成功 | 更新失敗・クラッシュ時の復旧、branch 作成の途中失敗、複数 Run の競合 |
| 高 | Portable Capsule v2/v3/v4 の受理・closure (`lib/objects`, `apps/portable-application`) | 形状、参照、digest、closure の入口を読解。object 側63件成功 | 全 profile での root と object kind の整合、未到達 object、外部 blob、サイズ、CLI/hosted の呼び出し境界 |
| 高 | Run admission と Contract receipt (`lib/runtime-attempt`) | 関連の既存 admission 9件と verification 15件成功。対象以外は未検査 | D/K 選択、権限・runtime 境界、全 observation と receipt の実行時結合 |
| 高 | Formation の source 取得・展開 (`lib/formation`, `apps/formation-worker`) | 展開の入口を読解。メモリ内圧縮展開に下記 U1 の要調査事項 | メモリ版とファイル版の入力経路、展開上限と deadline、symlink 連鎖、アーカイブ衝突 |
| 高 | Model Set の受信・キャッシュ (`lib/runtime-attempt/src/data_plane`) | range と digest 検証の一部を読解。形式モデル・追加再現は未実施 | 同時受信、resume、部分書込み、quota、取得先と grants の結合 |
| 高 | 通信の認可・egress (`services/netd`) | 前回の既知問題は修正済み。今回の新規モデルは未追加 | 並行予算、時計変動・再起動後の replay、DNS と接続の一貫性 |
| 中 | Semantic Core、Kernel、Composition (`lib/computation`, `lib/kernel`, `lib/compose`) | 未着手 | identity、Port、Evolution、配線・閉包の不変条件 |
| 中 | CLI lifecycle、Materializer、local Instance (`apps/cli`, `extensions/materializers`, `apps/portable-application`) | 一部の呼び出し元を読解したのみ | init/resume/stop、競合、保存・復旧、v2/v3 分離 |
| 中 | Coordinator、Formation Worker、Hosted Runner | 未着手 | Job/lease/attempt の帰属、取消・冪等性、実環境の権限境界 |
| 中 | ato-api、ato-pwa、Desktop、配信・外部連携 | 未着手 | 各 repo の契約・実装・認証・変更経路 |

### B1 — Record の開始位置を照合せず Run を進める（未修正）

`LocalCapsuleRepository::commit_observation` はトークン・`expected_head`・stream を検査するが、`record.head_before == expected_head` を検査しない。既存の正しい Record の後に別の `head_before` を持つ Record を渡すと受理して Run cursor を更新し、その後の `records_for_causal_branch` は `CausalHeadMismatch` になる。accepted RFC `LOCAL_CAPSULE_REPOSITORY.md` が示す Record の連続性と実装の不一致として扱う。

Lean: `RecordContinuity.mismatched_record_BUG`。実コード再現: `repro/record_continuity.rs` を `run.sh` から実行し、受理と履歴検証失敗を確認した。これは公開 repository API の不正入力での再現であり、通常の CLI の `RepositoryObservationSink::emit` は同じ locked head を両方に設定する。通常操作や外部利用者がこの不整合を誘発できるかは第2回で追記した（下記「B1 の追記」）。プロセスクラッシュや実機の再開操作は未検証。

### U1 — メモリ内 source archive の展開量（第2回で判定: 資源消費の懸念。確定バグには数えない）

`lib/formation/src/source.rs:57-73` のメモリ内 `decompressed` は gzip を `GzDecoder::read_to_end`、zstd を `zstd::stream::decode_all` で
**上限なしに Vec 全体へ展開**する。`measure()`（同 943-946）は `SourceLimits` を見ずにこれを先に呼び、その後に tar を検査する。
`TreeVerifiedArchive::materialize` も `expand_archive`（同 1050-1057）で再度全体展開する。
対して file 版 `file_reader`（同 598-622）は `ArchiveReadLimit` で展開後バイトを
`max_total_bytes + max_files*2048 + 1MiB`（既定 640,319,488 B = 610.7 MiB）に制限し、zstd は `window_log_max(27)` を明示する。
圧縮入力側は intake で `min(指定, max_total_bytes, MAX_SOURCE_OBJECT_BYTES=256MiB)` に制限される。

高圧縮データを `.tmp` 配下で生成して実測した（リポジトリは汚していない）:

| 生成物 | 生サイズ | 圧縮後 | 比 |
|---|---|---|---|
| ゼロ 64MiB → gzip -9 | 67,108,864 B | 65,260 B | 1028.3 : 1 |
| ゼロ 64MiB → zstd -19 | 67,108,864 B | 2,077 B | 32,310 : 1 |

intake 上限 256MiB を当てると、gzip で理論上約 257GiB の Vec、zstd ではさらに大きい Vec になり得る。
上限は展開後の `SourceLimits` でしか効かない。

**確定バグに数えない理由（到達条件）**: メモリ版に届く本番コードは repo 内で 2 箇所だけで、いずれも圧縮 bytes が攻撃者入力でない。
(1) `apps/formation-worker/src/retained.rs:73-110` は自プロセスが pack した artifact を再測定する（`pack.rs:42-66` は 8MiB 以上でだけ gzip 化）。
(2) `apps/formation-worker/src/local.rs:139` の `InitialCondition::Archive` は repo 内では `examples/` だけが構築し、
CLI の `ato form`（`apps/cli/src/lib.rs:1452-1515`）は `LocalDirectory` を使う。
外部から来る圧縮 archive はすべて file 版を通る（`job/source_intake.rs`、`source_archive/http.rs`、`runtime_network.rs`、
`lib/runtime-attempt/src/retained.rs`、`source_oci.rs`）。`.capsule` 内の source ZIP は `zip.rs` が entry 単位・合計で
`MAX_EXTRACTED_BYTES` に制限する。**攻撃者が圧縮 bytes を直接制御してメモリ版へ到達する経路は repo 内で見つからない**。

ただし `DownloadedArchive::new` / `measure_source_tree` / `verify_tree_digest` は pub の library API であり、
埋め込み利用者や将来の呼び出し追加が untrusted bytes を渡せば即座に成立する。file 版にはある境界がメモリ版に無い非対称は残る。

- 影響: 到達すればプロセスの OOM / abort（CLI ならローカル、worker なら attempt 失敗）。digest・closure の検証は破れない。
- Lean: 資源量の見積りであり純粋な判定ではないためモデル化していない（この判断自体を限界として記録する）。
- 未検証: メモリ版を直接叩く OOM 再現、zstd crate の既定 `windowLogMax` の実測、埋め込み利用者の有無。
- 修正候補（実施はしない）: メモリ版にも `ArchiveReadLimit` 相当の上限を付け、展開を file 版と同じ 1 箇所へ寄せる。

## 第2回 棚卸し（対象 0cb26f6f6、2026-11-17）

第1回で未着手だった Semantic Core / Kernel / Composition / CLI を対象に、Lean モデルと実コード再現で確認を進めた。
対象は branch feat/lean-bugcheck（未 push・PR 未作成）。今回も発見の棚卸しだけで、修正・PR 作成・外部通知は行っていない。
以下は確認できた範囲の記録であり、4 機能の全性質を網羅したものではない。

### 今回の棚卸し表（第2回で確認した範囲）

| 優先 | 機能 | 今回の確認と状態 | 次に調べる範囲 |
|---|---|---|---|
| 中 | Semantic Core (`lib/computation`) | **B8 を確認**: authoring の seal ゲートが `capsule.compose@1` 検証より弱く、seal できるが所有 semantics が拒否する Capsule を作れる（実コード再現）。Connection 正規化・boundary map・resolve 信頼境界・識別子正準性は Lean で確認 | authoring residual の正準性（F2）、residual の hash 要件（F3）、実 `ato run` E2E |
| 中 | Kernel (`lib/kernel`) | **B2 を確認**: 重複登録がエラー時にも既存登録を置換。Lean 反例と公開 API での再現あり。生産は固定 3 登録・Run ごと生成・握り潰し 0 件で**現行経路では未到達**（潜在） | Evolution の決定性、offer/action 検証、transition sink 失敗時の扱い |
| 中 | Composition (`lib/compose`) | **B3 を確認**: 深さ予算が DAG の高さを制限しない。既定 `max_depth = 64` で、最長経路 65 の DAG が `NodeId` 順序で accept/reject に分かれ、`Kernel::enabled`（生産の検証経路）でも同じ（attempted 65 / limit 64） | 閉包・role/protocol 検査、evaluation 側の資源上限、`ValidatedComposite` の利用側 |
| 中 | CLI lifecycle (`apps/cli`) | **B4-B7 を確認**: 拒否される resume が復元を先行し未保存変更を上書き（4 経路で再現）、restore が symlink 経由で project 外へ書く、stale lease を回収できない、`encap -o` が無確認上書き。v3 Derivation 選択は規則一致（否定） | init/stop の同型順序、同時 resume の相互排除、実 crash での lease 残留 |
| 高 | Run の記録とローカル保存 (`lib/objects`) | B1 の到達範囲を追加確認: 通常の CLI / adapter 経路では `head_before` が必ず `expected_head` と一致するため構造的に不達（下記「B1 の追記」） | 変更なし |
| 高 | Formation の source 取得・展開 (`lib/formation`) | U1 を判定: メモリ版の展開は上限なしだが、攻撃者が圧縮 bytes を制御して到達する経路は repo 内に無い（資源消費の懸念として保留） | 埋め込み利用者、pub API の将来利用、zstd 既定 window |
| 高 | Model Set の受信・キャッシュ (`lib/runtime-attempt/src/data_plane`) | range / resume の範囲計算と quota の予約・ピーク上界を Lean で証明。成長ゼロ import が ceiling 超過キャッシュで拒否される反例 1 件（候補・CLI のみ到達・Rust 再現は未実行） | quota の意図（総量か成長か）、配信経路に上限が無いことの是非、同時実行 |
| 高 | Run admission と Contract receipt (`lib/runtime-attempt`, `lib/formation`) | AGENTS.md の v3 不変条件と一致（選択 D のみ能力判定、stage 分離、K と verdict の 1:1、空 K 拒否、受理側の再計算）。Detected mismatch なし | receipt 生成側 API の将来利用、hosted の受理中継、実 HTTP/実コンテナ |

上表以外の行（portable capsule、runtime-attempt、formation source、data plane、Coordinator/Hosted Runner、ato-api/ato-pwa/Desktop）は第1回の記載から進展なし（第3回で更新した。下記「第3回 棚卸し」を参照）。

環境上の制約: この作業 worktree のサンドボックスは `ps` を EPERM で拒否するため、`lib/local-execution` の macOS 実装が
process start time を取得できず durable Run の worker が ACTIVE になれない。したがって「run が動いている状態」を前提とする挙動
（成功する resume、stop の kill→capture 順序、boot session 照合）は実行では確認できず、読解または合成 lease での再現に留まる。

### B2 — Kernel の重複登録がエラー時にも既存登録を置き換える（未修正）

`lib/kernel/src/lib.rs:494-511` の `register` / `register_protocol` は
`self.semantics.insert(id, semantics).is_some()` で**挿入してから**重複を判定し、
`KernelError::DuplicateSemantics` / `DuplicateProtocol` を返す。`BTreeMap::insert` は既存値を置換して旧値を返すため、
エラーを返した時点でレジストリは新しい登録に差し替わっている。`Kernel` は登録実装の同一性（版・ハッシュ）を持たず
`SuccessorSemanticsMismatch` も id 比較だけなので、この不整合を検出する機構が無い。

- Lean: `KernelRegistration` を「レジストリ = id → 実装マーカー」の関数モデルで表現し、
  契約 `Atomic`（「エラーを返したら状態は変わらない」）の否定 `code_register_not_atomic`、
  `duplicate_replaces_BUG`（Err 後も `enabled` が見る実装が変わる）、`replacement_visible`、
  `register_protocol` 同型の `protocol_duplicate_replaces_BUG` を固定した。
  修正版（`entry` / `contains_key` を先に置く）について `fixed_register_atomic`、`fixed_error_keeps_offers`、
  `fixed_success_registers` を証明した。抽象化で捨てたもの: `BTreeMap` 実装、`Arc`、`Send + Sync`、並行性、Semantics の内部。
- 実コード再現: `repro/core_boundaries.rs`。公開 API で同じ `SemanticsId` を二重登録すると `Err` が返り、
  その後の `Kernel::enabled` が**置換後の** semantics の offer を返し、`register_protocol` も
  `SemanticHost::roles_compatible` が置換後の `allows=true` を返すことを assert している（`run.sh` で実行）。
- 到達条件: `Kernel::new` の生産側呼び出しは `apps/connected-realization-worker/src/lib.rs:4760` の
  `hosted_evolution_kernel` のみ（他はテスト）。固定 3 件（compose / browser computation / browser protocol）を
  `?` で登録するだけで、Kernel は Run ごとに生成され、構築後に外から登録する経路は無い。
  `let _ = register` / `.ok()` / ループ登録 / データ駆動登録は 0 件（grep）。**現在のバンドル・CLI 入力からは到達不能**。
  同一プロセスの library 利用者や将来の拡張ローダが重複 ID を渡した場合に顕在化する潜在バグ。
- 同型（C1、読解のみ）: `lib/objects/src/bundle.rs:65,84`、`portable_bundle.rs:207`、
  `extensions/adapters/api/src/lib.rs:307,757`、`extensions/materializers/api/src/lib.rs:132,425` にも
  「insert してから Err」のパターンがある。各 registry の使われ方の評価は未実施。
- 未検証: 重複後の `step` 実装差し替えによる遷移内容の変化（観測したのは `enabled` と `roles_compatible` のみ）。

### B3 — Composition の深さ予算が DAG の高さを制限しない（未修正）

`lib/compose/src/validate.rs:240-295` の `validate_iteratively` は `Visit::Enter` で
① `check_depth(depth)` → ② `validated` なら subtree ごと skip → ③ `active` なら `ReferenceCycle` の順に判定する。
共有子が浅い経路で先に validated になると、より深い経路から到達しても子孫を再訪しないため、
DAG の root-to-leaf 高さが `max_depth` を超えても受理される。再訪したノード**自身**の深さは検査される（①②の順）ため、
隠れるのは「その子孫の深さ」である。判定は `NodeId`（作者が選ぶ識別子）の辞書順に依存する。

- Lean: `ComposeDepth` を明示スタック + `Enter`/`Exit` + `active`/`validated` 分離のモデルに置き換えた。
  反例 `order_dependent_depth_BUG` は「受理されるが `Spec`（root からの全経路が予算内）を満たさない」形で固定し、
  `accepts_shallow_first` / `rejects_deep_first`（同じ DAG、NodeId の a/b 入れ替えのみ）、
  `over_budget_path`、`listed_nodes_BUG`（root が全 chain node を直接持つ形）を持つ。
  モデルの忠実性は `revisited_node_itself_checked`（①②の順序）と `cycle_rejected` で確認した。
  修正版は DAG 高さで判定し、`height_bounds_path` と `fixed_sound`（fuel が全経路を覆えば、高さが予算内 ⇒ `Spec`）を
  一般形で証明した。`fixed_rejects_shallow_first` / `fixed_rejects_deep_first` / `fixed_pair_order_independent`。
  抽象化で捨てたもの: `ComputationRef` の内容アドレス性、residual のデコード、boundary/export/connection/role 検査、並行性。
- 実コード再現: 2 本。
  1. `repro/kernel_depth_default.rs`: **既定予算 `max_depth = 64`** で、最長経路 65 の DAG が `NodeId` の a/b 入れ替えだけで
     accept / reject に分かれる。さらに **`Kernel::enabled`（`ComposeSemantics` 経由 = 生産の検証経路）** でも同じ差が出て、
     拒否側は `validation resource limit exceeded for compose depth: attempted 65, limit 64` になる。直列チェーンは拒否される。
  2. `repro/core_boundaries.rs`: root が 70 個の chain node をすべて直接子に持つ構成（71 edge）を既定予算が受理する。
- 既存テスト `iterative_validator_handles_deep_closure_within_explicit_budget` と
  `depth_limit_is_reported_as_resource_exhaustion` は線形 chain のみを検証しており、共有子を持つ DAG は未カバー。
- 到達条件: `validate_composite`（既定予算）は `ComposeSemantics` の `validate`/`enabled`/`step` から呼ばれ
  （`lib/compose/src/semantics.rs:30`）、`derive_transition` は successor を再検証する。入力はバンドル由来で
  `NodeId` の綴りは作成者が決められる。`max_unique_computations = 4096` があるため経路長の上限は約 4096。
- 影響: closure / role / export / protocol の**受理意味論は破られない**。破れるのは「予算が全経路に効く」性質と、
  同じ形状の app が名前替えで通る再現性（監査・受入の観点で悪い）。下流の `collect_runtime_projection`
  （`apps/runtime-object-graph/src/lib.rs:482-525`）と `collect_browser_leaves`（worker:590-613）は深さ無制限に再帰するが、
  現状は unique 4096 が実質上限でスタック溢れには届きにくい（未検証）。ingest を大きい `max_depth`（テストでは 512）で
  通した caller が同じ graph を Kernel に渡すと既定 64 で拒否される（現状その caller はテストのみ）。
- 修正案: `Visit::Exit` で子の高さ `h` を memo し、`validated` skip する `Enter` では `depth + h - 1 > max_depth` を検査する。
  1 パス・線形のまま DoS 耐性を落とさない（`validated` skip を単純に外すと diamond で指数的になるので不可）。
- 未検証: 深い DAG を実際に評価したときのスタック使用量・失敗モード、ホスト型 runner のスタック設定。

### B4 — 拒否された resume が未保存の workspace 変更を上書きする（未修正）

`apps/cli/src/lib.rs:1547-1602` の `resume` は、selector 解決と runtime state 読み込みの後、
**`restore_workspace`（1553-1557）を先に実行**し、その後に `--branch` の重複（1563-1565）、
名前検証、historical point の検証（1583-1588）、lease 取得（1592）を行う。
したがって失敗する全経路のうち**復元より後に判定されるもの**では、コマンドが失敗しても
project の作業ツリーは選択した computation の snapshot 内容へ書き換わっている。

- 実コード再現: `repro/cli_resume.sh` と `repro/cli_lifecycle_order.sh`（どちらも `run.sh` で実行）。
  隔離 `OUT` と隔離 `ATO_HOME` で、次の 4 経路すべてで `state.txt` が `unsaved change` から `snapshot` に戻ることを確認した。
  (1) 既存 `--branch main`、(2) `--branch feature/x`（`/` を含む不正名）、(3) worker が ACTIVE になれない起動失敗、
  (4) stale lease による拒否。refs と `runs/active.json` は不変、snapshot に無い新規ファイルは残る。
- 復元は成功経路でも不要な先行副作用: 1553 の結果は 1558-1591 のどこからも読まれず、1592 の `start_durable` が起動する
  worker が `supervisor.rs:246-277` で同じ snapshot をもう一度 restore する。replay materializer の `encode` は
  作業ツリーを読まない。よって「検証してから復元」または「1553 を削除」が最小修正になる。
- Lean: `CliResumeOrder`。検査結果を `List Bool`、作業ツリーを `String → Nat`（snapshot が名指しするファイルだけ書く）として、
  `code_rejection_writes_named` / `code_rejection_changes_workspace` / `rejection_overwrites_BUG` / `code_keeps_unnamed` を固定し、
  修正版 `resumeFixed` について `fixed_rejection_preserves` / `fixed_never_writes_on_any_rejection` / `fixed_success_restores`、
  復元が union 意味論であること `resumed_workspace_exceeds_snapshot`、成功時も上書きすること `success_also_overwrites` を証明した。
- 到達条件: `ato resume <dir> --branch <既存名>`、`--branch feature/x` のような見慣れた名前、historical point を
  `--branch` なしで試す（エラー文が `--branch` を促すので 2 回目に起きる）、run を起動できない環境。いずれも終了コードは非 0。
- 影響: 利用者の未保存の作業ツリー変更が「エラーで何も起動しなかったのに」失われる。CLI 側に undo は無い。
  既存テスト `apps/cli/tests/computation_architecture.rs:571-575` は historical 経路を通すが、巻き戻しは assert していない。
- 未検証: durable Run が生きている状態での resume（この環境では `ps` が EPERM で worker を ACTIVE にできず、
  active Run 競合は合成 lease でのみ再現）、historical point 経路そのものの CLI 実行、Windows / Linux の process identity。

### B5 — restore は destination の symlink を辿って project 外へ書く（未修正）

`extensions/adapters/workspace/src/lib.rs:56-83` の `restore_workspace` は、`checked_path`（同 268-278）で
相対パスの**文字列**だけを検査し、destination 内の symlink を解決しない。`create_dir_all` / `fs::write` は symlink を辿る。
capture 側（同 241-243）は symlink を `WorkspaceError::Symlink` で拒否しており、境界の扱いが非対称。

- 実コード再現: `repro/cli_lifecycle_order.sh` の BUG 5。snapshot 取得後に `sub/` を外部ディレクトリへの symlink に
  差し替えると、`ato resume` が project 外の `outside/data.txt` を snapshot 内容で作成することを確認した。
- 到達条件: 作業ツリーに symlink ディレクトリがあり、snapshot がその配下のパスを含むこと。通常の `init` は symlink を拒否するため、
  snapshot 取得後に対象が symlink へ置き換わる必要がある（対象ツリーへの書込み権限が前提）。
- 影響: 「workspace boundary の内側にだけ書く」不変条件の破れ。project 外の既存ファイルを上書きし得る。
- 未検証: `create_dir_all` だけが外へ出るケース、深い symlink 連鎖、Windows の junction。

### B6 — stale active Run lease を回収する経路が無い（未修正）

`lib/objects/src/repository.rs:803-818` の `claim_active_run` は `runs/active.json` が存在すれば常に `ActiveRunConflict` を返し、
`lib/local-execution/src/supervisor.rs:370-391` の `stop_active` は status が `active` で、かつ記録した process identity
（boot session + start time）が一致する場合だけ停止する。crash・再起動・`ato stop` の途中失敗で残った lease を消す
コマンドは無く、以後その project は resume も stop もできない（手動で `.capsule/runs/active.json` を消す以外に回復手段が無い）。

- Lean: `LeaseRecovery.claim_blocked_by_any_lease`、`stale_lease_blocks_stop`（status が何であれ identity 不一致なら拒否）、
  `matching_process_can_stop`、`fresh_lease_is_claimable`。
- 実コード再現: `repro/cli_lifecycle_order.sh` の BUG 4 / 4b（`ActiveRunWire` 形式の合成 lease）。
  resume は `active Run lease is already held`、stop は `process identity no longer matches` で拒否される。
- 到達条件: `ato stop` の途中失敗、CLI の強制終了、マシン再起動、worker の異常終了。
  `start_durable` は自身の失敗時に lease を解放する経路があるため、その経路だけは残留しない。
- 影響: B4 と組み合わさると「拒否されるたびに作業ツリーが巻き戻り、回復コマンドも無い」最悪形になる。
- 未検証: 実 crash での残留（本再現は合成 lease）。

### B7 — `encap -o` が既存出力を無確認で上書きする（未修正・軽微）

`apps/cli/src/lib.rs:1613-1635` の `encap` は出力の存在確認をせず `atomic_write` する。`export`（2560-2562）と
`reform`（3213-3218）は既存出力を拒否するので非対称。実コード再現で、12 バイトの既存ファイルが bundle に置換されることを確認した。
影響は利用者指定の出力先が失われることのみ。

### B8 — authoring の seal ゲートが `capsule.compose@1` 検証より弱い（未修正）

`capsule.toml` の配線を検査する `load_config`（`lib/local-execution/src/authoring.rs`）は、接続ごとに
「from != to・両端が宣言済み・両端 internal・protocol 一致」だけを見る。`seal_authored_source` → `seal_composite` は
そのまま `capsule.compose@1` の `ComputationObject` として seal し、**validator を呼ばない**。
一方 `lib/compose/src/validate.rs` の `validate_local` は同じ配線を拒否する。

| witness | `load_config` | compose@1 validation |
|---|---|---|
| W1 同じ `[[connection]]` を 2 回 | 受理 | `DuplicateConnection` |
| W2 同一 process（node）の 2 Port を接続 | 受理 | `SameNodeConnectionUnsupported` |
| W3 1 個の internal client Port を 2 個の server に接続 | 受理 | `EndpointBoundMoreThanOnce` |

- Lean: `WiringGate.authoringAccepts` / `composeAccepts` を分けて witness を `by decide` で固定し、
  `seal_without_validate_BUG`（authoring は受理するが compose 側は拒否する配線が存在する）、
  `linear_endpoint_not_enforced_at_seal`（W3 の形）を証明した。seal 前に compose 側の endpoint 検査も通す修正版について
  `fixedAccepts_sound`（seal された ⇒ 検証を通る）を証明した。抽象化で捨てたもの: protocol は `Nat` の一致比較、
  role 互換性は常に true、exports と boundary の対応検査、`Connection` の derived Ord。
- 実コード再現: 2 本。
  1. `repro/semantic_compose_wiring.rs`: `authoring::seal_composite` と同じ手順で seal し、W1-W3 が
     「seal 成功 → validation が拒否」になることを確認（`run.sh` で実行）。
  2. `repro/semantic_authoring_gate.rs`: 実 `load_config` と公開 `adapter_instances` で W1-W3 の受理を確認。
     positive control として自己接続と未配線 client が拒否されることも確認。
- 実害（W3）: `[[connection]]` の宣言順を入れ替えるだけで、process a の環境変数 `PEER` が
  `127.0.0.1:7002` / `127.0.0.1:7001` と変わる（`process_environment` が宣言順に上書きし、エラーを出さない）。
  どの peer に繋がるかが記述順で決まり、利用者へ通知されない。
- 到達条件: `ato init` / `ato run` の authoring 経路はすべて `load_config` → `seal_authored_root` →
  `seal_authored_source` → `seal_composite` を通る。`validate_composite` の呼び出し元は `ComposeSemantics` とテストだけで、
  `ComposeSemantics` を登録するのは `apps/connected-realization-worker/src/lib.rs:4761` のみ。
  `lib/local-execution/src/registry.rs` は Semantics を登録しないため、**CLI の authoring 経路では compose 検証が一度も走らない**。
  role の向き（client/server）も authoring では検査されない。
- 影響: `COMPOSITION.md` が述べる「linear endpoint use / 同一 node 拒否 / role 互換性」を authoring が作る Capsule では保証されない。
  一度 seal した `ComputationRef` は不変なので、後から `load_config` を厳しくしても既存 branch の head は残る。
  W1 は同じ配線に 2 つの `ComputationRef` ができ、identity が増殖する。
- Detected mismatch: `COMPOSITION.md` の検証記述は `ato-compose` validator の性質で、`lib/local-execution` の seal 経路は
  その validator を通らない。どちらを正とするか（seal 時検証 / `load_config` 強化 / 両方）は設計判断で、修正はしていない。
- 未検証: 実 `ato run` / `ato init` を W1-W3 の `capsule.toml` で最後まで実行する E2E（プロセス起動が必要）、
  hosted worker 側で同じ Capsule が `validate` で落ちるか、CLI が compose Capsule を Kernel に通すか。
### 候補（未実行・コード読解のみ。確定バグに数えない）

- F3: `restore_workspace` は「snapshot への復元」ではなく「snapshot との union」（削除処理が無い）。resume 後の作業ツリーは
  record された状態ではなく「record ∪ 残骸」になり、`stop` の `evolve_workspace` がその union を次の head として capture する。
  accepted RFC `CAPSULE_CLI_LIFECYCLE.md:23-27` は union を明示していない（Detected mismatch 候補）。
- F4: `stop` は quiesce → kill → capture の順で、capture 失敗時にプロセスだけ死に、head 不進・lease 残留になる。
  この状態は B6 の回復不能状態に一致する。run を起動できない環境のため未実行。
- F7: `runs/stop.request` は成功時のみ削除され、失敗経路で残ると次の resume が起動直後に停止処理へ入る。読解のみ。
- C2: 子 semantics が successor で boundary を変えると、`enabled` に出た offer が `step` で導出不能になり得る。
  in-tree semantics は boundary を保存するため未到達。
- C10: Kernel 経由の export role 判定は `parent == child` 固定で、`validate.rs` の任意 `ProtocolRolePolicy` と食い違う。
- B9: `lib/runtime-attempt/src/data_plane.rs:557-589` の `ensure_bounded` は import の**成長**ではなくキャッシュの**総量**を上限と比較する。
  上限は CLI の per-invocation フラグ（`--max-cache-bytes`、既定 64GiB）でキャッシュに保存されないため、
  以前に大きい上限で取り込んだキャッシュへ上限を小さくして再 import すると、書き込みゼロの import が
  `model_cache_quota_exceeded: import needs N bytes, limit is M` で失敗する（Lean:
  `DataPlaneQuota.no_growth_import_refused_when_over_ceiling`、`the_gap_is_the_growth_condition`）。
  到達は `apps/cli/src/model_cache.rs:101`（`ato model import`）のみ。Rust 再現は未実行で、意図（総量上限か成長上限か）も未確認。
- F2: authoring residual（`AuthoringState` を JCS 化したもの）は `Vec` の宣言順を保存するため、同じ意味で宣言順だけ違う
  `capsule.toml` が別の `ComputationRef` になる。加えて `[encap] materializers`（realization 要求）が identity に入り、
  `OBJECT_BUNDLE.md` の「materialization は identity を変えない」と緊張する。再現は rlib 版不一致で断念（未検証）。
- F3: `ComputationObject.residual` は blake3 と sha256 の両方の `ContentRef` を受理し得る一方、`compose` の `residual_size` は
  blake3 を要求する。residual の hash 要件が経路ごとに違う（identity の強度ではなく一貫性の問題）。影響未評価。
- receipt の**生成側** API（`ContractVerificationReceipt::new` / `from_runtime` / `for_attempt`）は渡された verdict 集合を検査しない。
  現行の呼び出し元は `verify_runtime` の結果だけを渡すので問題ないが、将来 K と無関係な verdict を渡す呼び出し元が増えると
  `fully_satisfied` を作れてしまう（守っているのは受理側 `accept_receipt`）。

### B1 の追記 — 通常 CLI 経路からの到達性

`lib/local-execution/src/supervisor.rs:934-974` の `RepositoryObservationSink::emit` は、
ロックした `head` を `expected_head` と `record.head_before` の**両方**に渡している。
`commit_observation` の検査は `expected_head` と `run.head` を比較するため、CLI / adapter 経路では
`head_before` の不一致が構造的に発生しない。よって B1 は公開 repository API の不正入力でのみ成立し、
通常操作・外部利用者からは到達しない、という前回の記載をコード上で確認した（実行による追試は未実施）。

### 第2回で確認した性質（成り立つと確認）・否定した疑い

- `lib/computation/src/lib.rs` の `Connection::new` は endpoint を正準順序へ並べ替え、自己接続を拒否する。
  順序非依存性は自己接続のエラー分岐込みで証明した（Lean: `ConnectionNormalization.newConn_comm` / `newConn_self`）。
- `Boundary = BTreeMap<PortId, PortDef>` なので 1 つの PortId が二重定義されることは構造的にない
  （`BoundaryIdentity.boundary_has_one_value`）。二重バインドが問題になるのは connections / exports 側だけ（B8 の W3）。
- resolve の信頼境界は十分: `read_exact_object` の `.take(expected + 1)` は「ちょうど expected バイト」以外を受理せず
  （`ResolveTrust.take_one_past_accepts_only_exact`）、受理 ⇒ 長さ一致 ∧ digest 一致（`accepts_implies_digest`）。
  信頼できない resolver がサイズを偽っても identity 検証は迂回できない（BLAKE3 の衝突耐性は仮定）。
- 識別子は component 内に `@` と大文字を持てず ASCII のみ（`IdentifierSyntax.valid_segment_no_at` / `no_upper`）。
  したがって `rsplit_once('@')` は曖昧でなく、JCS の UTF-16 code unit 順と `BTreeMap<String>` のバイト順も一致し、
  boundary のキー順が実装依存で揺れない。空 segment が無いことも証明した。
- Bundle closure が composition residual の content を落とす疑いは**否定**（`lib/objects/src/bundle.rs:354` が
  `ObjectLink::Content(residual)` を追加している）。
- `apps/cli/src/derivation_selection.rs` の選択規則は v3 の不変条件と一致: 明示指定は置換されず（`explicit_never_replaced`）、
  複数宣言かつ未指定なら 1 つも実行しない（`multi_executes_nothing` / `no_receipt_without_selection`）、
  暗黙に選ばれるのは単一宣言のときだけ（`single_executes_only`）、receipt は実行した D を指す（`receipt_names_selected`）。
  CLI 側の許可表示が単一 D で省略される点は、admission が `portable_run.rs:387-409` で必ず走るため規則違反ではない（**否定**）。
- Run admission と Contract receipt は AGENTS.md の v3 不変条件と一致（Detected mismatch なし）:
  能力判定は選択した D の `AttemptSpec` だけ（`admission.rs:128-326`、呼び出しは `attempt.rs:224` の 1 箇所）、
  authoring / preset / projection / admission / build / verification は stage 分離、`verify_runtime` は K の requirements を
  1:1 で判定して未知 verifier は Failed、空 K は authoring と bundle validator が拒否（真空合格なし: `authoring.rs:1369-1376`）。
  受理側 `accept_receipt`（`lib/formation/src/receipt.rs:89-231`）は observation 集合 = K の requirement 集合（欠落・余剰・重複を拒否）、
  `fully_satisfied` を観測から再計算して不一致を拒否、実行 receipt の Deferred を拒否し、`derivation_ref` を canonical D と照合する。
  receipt の生成側 API が verdict 集合を検査しない点だけは将来利用への注意として残す（上の候補）。
- `lib/runtime-attempt/src/data_plane/ranges.rs` の range / resume 計算は隙間・重複・欠落を作らない:
  範囲長 = `min(chunk, total - start)`（`range_length_exact`）、連続 start は chunk 差（`next_start_is_adjacent`）、
  途中再開の prefix は chunk 整列不要（`first_range_starts_at_the_prefix`）、バッチ合計は `min(4*chunk, total - have)`
  （`batch_covers_the_uncovered_prefix`）。
- `data_plane.rs:557-589` の quota 判定は安全側: 予約は確定後 usage の上界で（`projected_bounds_committed`）、
  未計上の `.partial.range-N` を含むピークも確定後 usage を超えない（`peak_never_exceeds_committed`）。
  「未計上の一時ファイルで上限を超える」という当初の疑いは**否定**した。
- Composition の他の疑いは否定: `reserve_bytes` の `saturating_add` は飽和して必ず拒否側に倒れ（オーバーフローで通らない）、
  `boundary.keys().eq(exports.keys())` は BTreeMap 昇順なので集合一致として正しく、connection と export は同じ
  `bound_endpoints` で二重束縛を検出し、unique computation 数に off-by-one は無い。`derive_transition` が失敗時に
  未到達 object を残すのはコード内に明記された設計（GC material）。`validate_action` / `UnknownPort` の網羅に穴は見つからない。

### 第2回の限界

- B8 は `load_config` と `adapter_instances` までで、実 `ato run` の E2E と hosted 側の挙動差は未確認。
- B2 は現行の生産経路では到達しない（潜在）。B3 は実害（スタック溢れ等）が未確認。B4 / B5 は同時実行と部分失敗が未確認。
  B6 は合成 lease での再現で、実 crash の残留は未検証。B8 は Rust 再現が未実行。
- Lean モデルは手書きで、`BTreeMap` の順序、ファイル I/O、時間、同時実行は捨象している。ComposeDepth は fuel = 経路長の仮定を持つ。
- U1 は Lean モデル化していない（資源量の見積り）。メモリ版を直接叩く OOM 再現も未実施。
- 今回見たのは 4 機能（Semantic Core / Kernel / Composition / CLI）と 3 行（Formation、data plane、admission/receipt）で、
  棚卸し表の残りの行は未着手のまま。全機能の網羅でも全バグの解消でもない。
## 第3回 棚卸し（残りの行、対象 024ac2a54、2026-11-18）

第1回・第2回で「未着手」だった行（Portable Capsule、Run 記録の復旧、netd の残り、Coordinator/Formation Worker/Hosted Runner、
Materializer/Instance、ato-api/ato-pwa/Desktop/配信）を対象に、棚卸しを一巡させた。**棚卸し表の全行に状態を付けた**が、
行ごとの深さは異なる（コード全域の読解ではなく、境界ごとの到達範囲の記録）。修正・PR・外部通知はしていない。

### 今回の棚卸し表（第3回で更新。全行の状態）

| 優先 | 機能 | 状態（今回の到達点） | 未検査 / 次の範囲 |
|---|---|---|---|
| 高 | Run の記録とローカル保存 (`lib/objects`, `lib/local-execution`) | **R1/R2/R3 を確定**（実コード再現あり）。crash 復旧・並行の候補は R4/R5 | 電源断の実測、ENOSPC/EIO、真の並行実行 |
| 高 | Portable Capsule v2/v3/v4 (`lib/objects`, `apps/portable-application`) | **P1 を確定**（v2 closure が Content/Computation を混同し、宣言 closure ≠ 真の closure のまま受理）。v3/v4 の dispatch・closure・digest は実コードで拒否を確認 | hosted import の実環境、Hosted v4 roundtrip |
| 高 | Run admission と Contract receipt | 第2回で一致を確認。今回 **B13**（ato-api の受理 schema が /1 固定で Rust の /3 を拒否）を追加 | 実 hosted import での再現、Control Plane 側の retry 判断 |
| 高 | Formation の source 取得・展開 (`lib/formation`) | U1 の判定を維持（到達経路なし・資源の非対称のみ） | 埋め込み利用者、zstd window |
| 高 | Model Set の受信・キャッシュ (`lib/runtime-attempt/src/data_plane`) | 第2回で範囲/quota の性質と B9 候補を確認 | 配信経路の上限、同時実行 |
| 高 | 通信の認可・egress (`services/netd`) | **N1 を確定**（DENIED_RANGES が IPv4 埋め込み 3 形式を欠く）。N5-N8 を否定 | 6to4/NAT64-local の実経路、gateway 実接続 |
| 中 | Semantic Core、Kernel、Composition | 第2回で B2/B3/B8 と性質を確認。今回の残りなし | 実評価スタック、C1/C2/C10 |
| 中 | CLI lifecycle、Materializer、local Instance | **B11/B12 を確定**。候補 6 件（下記） | durable Run 稼働中の挙動、vm-snapshot 本体 |
| 中 | Coordinator、Formation Worker、Hosted Runner | **H1-H4 を確定**。Coordinator（ato-api）は repo 外で未検査。H5-H8 候補 | ato-api の fence/retry/reaping、実 staging 6 slot |
| 中 | ato-api、ato-pwa、Desktop、配信 | ato-api は B13 と境界 probe、PWA は実行テスト 178 件、Desktop は IPC/navigation、配信 2 repo は manifest 受理境界とテスト 69 件を確認 | PWA 875 ファイルの残り、Desktop cargo test、ato-api 109 route の残り |

### B11 — `[encap] materializers`（realization 要求）が sealed identity に入る（未修正・F2 の確定）

`lib/local-execution/src/authoring.rs:948` の `seal_state` は `AuthoringState{config, workspace_snapshot}` 全体を JCS 化する。
`config` には `EncapConfig{materializers}`（同 120-125）が入るため、realization の要求だけが違う 2 つの `capsule.toml` は
別の `ComputationRef` になる。実コードで `[encap]` 以外同一の 3 プロジェクトを `ato init --initial-only` し、
`blake3:212a9bff…` / `f2fe5a5c…` / `45f73142…` と別 ref になること、residual 実バイトに `encap.materializers` が
入っていることを確認した。`encap --materialize <id>` で後から materializer を選んでも root は変わらないため、
「実体ではなく宣言だけが identity を動かす」非対称がある。MATERIALIZATION.md と AGENTS.md 不変条件 5
（materialization は identity を変えない）に反する。Lean: `MaterializerIdentity.request_changes_identity_BUG`、
`three_requests_three_identities`、修正版 `fixed_identity_ignores_request`、対照 `encap_keeps_root`。
到達: `ato init` → `seal_authored_root` → `seal_state`。未検証: `ato run <file.capsule>` の E2E、v3/v4 bundle の同種混入。

### B12 — durable local Instance の Starting lease が孤児化し、回復経路が無い（未修正）

`claim_run`（`apps/portable-application/src/lib.rs:1950`）から `activate_run`（同 2388）の間に親 CLI が死ぬと
（Ctrl-C・端末クローズ・OOM kill）、`active-run.json` が `starting` のまま残る。以後 `ato app start` は
`already has Run … in status starting`、`ato app stop` は `still preparing and cannot be stopped` で拒否され、
`active-run.json` を手で消すまで回復しない。token はディスクに平文で残る（再現で確認）ので、塞がっている理由は
秘密性ではなく「非 Active lease を release するコマンドが無い」こと。B6（authoring 側）と同型だが別ファイル・別コマンド。
Lean: `InstanceLease.starting_lease_is_absorbing`、`starting_lease_closed_to_user_commands`、健全側 `release_fences_by_token` ほか。
実コード再現: `repro/instance_lease.rs`（合成 `instance.json` + `claim_run` 1 回。`run.sh` で実行）。
未検証: 実プロセスを殺しての再現（`ps` EPERM で worker を ACTIVE にできない）。

### R1 — 欠落した Record を cursor 復旧が見逃し、seq 再利用で履歴が恒久に再生不能になる（未修正）

`atomic_create` / `atomic_write`（`lib/objects/src/repository.rs:1050-1079`）は file を `sync_all` してから rename するが
**親 directory を fsync しない**。cursor は別 directory（`runs/active.json`）にあるため、「cursor は durable、record の rename は消失」
という順序が起こり得る。`next_sequence`（888-906）は最大 seq + 1 で欠番を埋めず、`active_run` の復旧（779-799）は
cursor より後だけを見るので欠落を検出しない。結果、seq が再利用され append-only 列が非連結になり、
`records_for_causal_branch` が恒久 `CausalHeadMismatch` を返す（`ato resume` / `encap` / replay が不能）。
Lean: `RecordRecovery.lost_record_is_invisible_to_the_cursor` / `lost_record_sequence_is_reused` / `reuse_breaks_the_chain`。
実コード再現: `repro/record_recovery.rs`（record 2 を削除して再現。`run.sh` で実行）。
未検証: 実 FS の電源断での rename 消失（削除で代替）、ENOSPC/EIO。

### R2 — cursor 書込みだけ失敗すると sink が永久に停止する（未修正）

`commit_observation` は record を `atomic_create` した後に `atomic_write(runs/active.json)` する。2 つ目だけ失敗すると、
record は durable で `active_run()` は新 head を返すのに、`RepositoryObservationSink` の cached head は古いままなので
以後の `emit` が全て `ActiveRunConflict` になる（再同期経路が無い）。Lean: `RecordRecovery.cursor_write_failure_stalls_the_sink`。
実コード再現: `repro/record_cursor_write.rs`（`.capsule/runs` を 0555 にして EACCES を注入）。
未検証: ENOSPC/EIO、adapter が emit の Err をどう扱うかの全数確認。

### R3 — `create_branch` が origin を先に書き、失敗で orphan origin を残す（未修正）

`create_branch`（repository.rs:520-558）は origin → head の順に `atomic_create` する。head 側だけ失敗すると
`branch_origin()` は値を持つが `head()` は None という状態が残り、同名 branch の再作成は origin の AlreadyExists で恒久失敗する。
origin を渡さず作り直すと古い anchor が残り、先頭 Record で causal replay が壊れる。
Lean: `BranchOriginOrder.partial_failure_leaves_an_orphan_origin` / `orphan_origin_blocks_the_retry` / 修正版 `createBranchFixed`。
実コード再現: `repro/record_branch_origin.rs`（`refs/heads` を 0555 にして再現）。到達: `ato resume --branch X` の create_branch。

### N1 — egress の CIDR deny list が IPv4 埋め込み IPv6 形式 3 種を欠く（未修正）

`services/netd/src/egress/gate.rs` の `DENIED_RANGES` は `::ffff:0:0/96` を含むが、`::/96`（IPv4-compatible）、
`2002::/16`（6to4）、`64:ff9b:1::/48`（RFC 8215 local-use NAT64）を欠く。実ファイルから定数を抽出して実行し、
`::7f00:1`（::127.0.0.1）、`2002:7f00:1::`、`2002:a9fe:a9fe::`（169.254.169.254）、`64:ff9b:1::7f00:1` が
`denied=false` になることを確認した（positive control は denied=true）。
Lean: `EgressCidr.compat_loopback_allowed_BUG` / `sixToFour_loopback_allowed_BUG` / `nat64_local_loopback_allowed_BUG`、
修正版 `deniedFixed` で `fixed_spec`。既存の `Egress.no_loopback_or_linklocal` は `effectiveV4` が `some` の形式だけを対象にした定理で、
これらの形式は射程外（矛盾しない）。到達: allowlist のホスト名が DNS でこれらの形式に解決される場合のみ（低〜中）。
未検証: 実ホストが 6to4 / NAT64 local の経路を持つか。

### H1 — Hosted Runner が format / 期限 / commit 失敗を 1 つの wire code に畳み込む（未修正・Detected mismatch）

`apps/connected-realization-worker/src/lib.rs:2213-2243` は、`validate_lease` / `verified_spec` の不整合、
`max_duration` 到達、state commit 失敗をすべて `realization_failed` として報告する。中間 code は
`message.contains("execution authorization")` の**prose 一致**で決めており、`lib/formation/src/failure.rs:11-14` が
明示的に否定している方針（code を型で運ぶ）と正反対。AGENTS.md の「format invalid / admission failure /
execution failure / Contract failure を同じエラーへ畳み込まない」に対する Detected mismatch。
Lean: `HostedFailureCode.distinct_causes_share_the_wire_code` / `only_authorization_is_separable` / 修正版 `spec_code_is_injective`。
実行: `repro/hosted_lease_logic.rs`（判断関数を実ソースから逐語コピー。`run.sh` で実行）。
注意: Formation Worker 側は code+stage を型で運んでおり非対称。

### H2 — 宣言した max_duration 到達が「失敗」として報告され、slot が recovery 隔離に入る（未修正）

`wait_for_stop`（lease.rs:1393）は deadline 到達で bail し、呼び出し側は `realization_failed` + `mark_slot_recovered(false)` にする。
設計意図（lease.rs:114-118）は「cron/browser に依存しない cleanup」なので期限到達は想定経路だが、
control plane には失敗として届き、state は commit 済み（「失敗したのに durable state が進む」）、その slot は
recovery が stopped を届けるまで新規受付を止める。

### H3 / H4 — lease の `expires_at` が Run の寿命を縛らない / 定数の doc と実装が矛盾

`expires_at` を参照するのは `validate_lease`（lib.rs:3951-3956）の受理時 1 回だけで、比較はローカル壁時計。
Run の寿命は `maximum_lifetime` だけが決める。同じファイルの `ExecutionAuthorizationMonitor` は
「壁時計と比較しない」と明記しており、同一概念に 2 つの時計方針がある。また `RUNTIME_LAUNCH_MAX_DURATION_SECS` の
doc は「省略時は 1 時間 cap」と書くが、`execution_authorization` 付き lease では `maximum_lifetime` が None になり cap が無い。
Lean: `HostedLeaseLifetime.expiry_does_not_bound_the_run` / `cap_omitted_BUG`。
到達: `max_duration_secs` も execution_authorization も付かない runtime_launch lease（H3）、authorization 付き lease（H4）。
未検証: ato-api が expires_at を回収に使っているか、実機の時計ずれ。

### B13 — ato-api の receipt 受理 schema が /1 固定で、Rust の /3（TLS 証拠）を拒否する（未修正・cross-repo）

Rust の hosted validator は `verify_observed_candidate` で receipt を作り、schema を
「native → /5、connector → /4、TLS 証拠あり → /3、…、それ以外 → /1」と決める（`lib/runtime-attempt/src/verification.rs:313-329`）。
`ato.contract.tls@1` requirement を hosted で観測する経路があるため（`validator_agent.rs:263-300`）、TLS を持つ app の receipt は /3 になる。
一方 ato-api の受理 schema は `schema: z.literal("ato.contract-verification-receipt/1")` 固定（`src/services/capsule_bundle.ts:553-576`）で、
ack route は `zValidator` なので /3 は 400 になり、validator は ack 失敗で停止する。
影響は fail closed（誤って verified にはならない）だが、TLS requirement を持つ portable app は hosted で verified に到達できない。
AGENTS.md の「CLI と ato.run は同一 schema の receipt を発行し」に対する Detected mismatch。
証拠はコード読解のみ（ato-api に node_modules が無く zod を実行できない）。未検証: 実 hosted import での再現、ato-api に /3 以降を受ける別経路があるか。
### P1 — v2 bundle の closure が Content と Computation を混同し、閉じていない closure を受理する（未修正）

`lib/objects/src/bundle.rs` の `closure`（527-582）は visited を 1 つの `result` map で表し、
`ObjectLink::Computation` の処理で `if result.contains_key(&content) { continue; }` する（558-562）。
ある computation の residual が別 computation の `content_ref` と一致すると、後者は Content として先に登録され、
Computation として訪れた時点で skip される。その computation は解決されず、子孫が closure に入らない。
`import_bundle`（494-515）は declared と **traversed** の両方向一致しか見ないため、この truncated closure を受理する。

- 実コード再現: `repro/bundle_v2_closure.rs`（公開 API で export → import。`run.sh` で実行）。
  観測: exported objects = 2（本来 3）、child kind = Content、import 受理、子の residual が destination に無い。
  対照として子を root に export すると 2 件（子 + 孫）になり、走査自体は壊れていない。
- Lean: `BundleClosureV2.v2_closure_drops_a_reachable_object_BUG` / `accepted_bundle_is_not_closed_BUG` /
  修正版 `the_fixed_traversal_is_closed` / `fixed_closure_keeps_it`。
- 到達条件: 公開 API では再現可。in-tree で `ObjectLink::Computation` を出すのは `ComposeReferences` だけで、
  compose root の residual（`CompositeResidual`）は `ComputationObjectWire` と schema が交わらないため、通常の authoring では不達（B2 と同格の潜在）。
  細工した compose + authoring 混在 graph では、BFS 順（`BTreeMap<NodeId>` 昇順）で Content が先に入り得る。
- 影響: `OBJECT_BUNDLE.md` の「Import verifies … complete closure」が破れる。identity の改ざん・権限昇格ではない。
  受理後に closure を辿ると `NotFound` になり、Run が後段で失敗する遅延失敗。
- 未検証: 実 `capsule.toml` から上記 graph を作る手順、hosted 経路での観測、`MAX_BUNDLE_OBJECTS` 近傍。

### v2/v3/v4 の受理境界（否定を含む）

- dispatch: version と profile は 1:1 で、取り違え 15 ケースを実コードで拒否確認（`repro/bundle_v3_shape.rs`）。
  拒否は digest を見る前の段で起きるため、後段の digest 失敗に救われているだけではない。
- v3 closure: declared == reachable を両方向で検査（未到達 → `UnreachableObject`、未宣言 → `IncompleteClosure`、
  未知 schema → `MissingExtractor`）。`repro/bundle_v3_closure.rs` で確認。
- payload の長さ/digest、厳密昇順 → 重複排除、root（contract_ref / derivation_ref）の再計算一致を確認（Lean `PortableShape`）。
- C1: `import_bundle` は canonical JCS を検査しない（`decode_bundle` のみ）。RFC の「Import verifies canonical JCS」との Detected mismatch だが、
  in-tree は decode 経由で未到達。
- C2: v2 descriptor の `kind` は内容と照合されない（読むコードが無く影響 0）。
- C3: v4 の external blob は長さ・digest を検証せず、宣言 size だけが予算を消費する（過剰拒否側）。
- C4: 宣言上限が同時に達成できない。512MiB の OCI archive の base64 は 715,827,884 文字で、768MiB の残りに 64MiB object が入らない
  （Lean `BundleBounds.per_part_limits_are_not_jointly_attainable`）。安全側。
- C5: `descriptor()` / `payload_bytes()` はソート済み前提の `binary_search` を公開 API として持つ（validate 後呼び出しが前提）。
- C6: `instance_snapshot_ref` は ato_objects 層では Structured を要求しない（上位層が塞ぐ）。
- C7: `decode_capsule_bundle_document` はサイズ検査より先に JSON 全体を走査し、`fs::read` に上限が無い（増幅は無い）。
- C8: v2 bundle は sha256 residual を運べる（F3 と同じ経路差。未実行）。
- 否定: v4 restore evidence は local 経路では実 bytes から再計算する。ただし **Hosted 経路は API が永続化した evidence を受け取る**ため、
  Rust validator は restore を独立観測しない（信頼境界の記録）。Hosted / CLI は v2 を capsule identity として扱わない。
### 外部 repo（ato-api / ato-pwa / Desktop / 配信）の確認範囲

これらは本 worktree の外（別 repo）であり、解決策の提示ではなく境界の把握に留めた。

- **ato-api**: B13（receipt schema の /1 固定）のほか、CORS/CSRF 境界の純関数（`isSameOriginRequest` / `canonicalizeOrigin` /
  `surfaceActivationProbeUrl`）を Node 22 で実行し、probe URL 19 入力で origin escape 0、origin の完全一致比較を確認した。
  認可は `requireAuth` / `requirePublisher` / `requireManagementAuth(scope)` / admin allowlist の 4 層で、admin は
  Cloudflare Access 優先・device credential 非受容・beta grant 再検査・空 allowlist で fail closed。
  未検査: 109 route の大半、D1 の所有権クエリの網羅、冪等性の実装、R2 signed URL、rate limit。node_modules 不在のため既存テストは未実行。
- **ato-pwa**: README は GitHub のサニタイズ済み HTML と、Codeberg の markdown を escape-first で描画する 2 経路。
  `npx vitest run src/auth src/lib/repoSource` が 178 件成功。セッショントークンを localStorage に置く箇所は見つからない。
  未検査: 875 ファイルの大半、E2E、配信物の CSP。
- **ato-desktop**: `#[tauri::command]` は 7 個、`capabilities/default.json` は `main` window のみ。
  `navigation.rs` の `classify` は userinfo を常に拒否し、Main は local launcher URL のみ、guest は exact loopback origin のみ Allow する純粋関数で、
  既存 unit test が retired remote Home の拒否と origin 固定を assert している（読解。cargo test は未実行）。
- **配信（ato-edge / ato-usercontent-static）**: `parseStaticManifest` は JCS 再 encode 一致 + sha256 digest + 厳密 key 集合 +
  固定 `frame_ancestors`（4 origin の固定順）+ ASCII 昇順 `connect_src`（https/wss の bare origin のみ、
  localhost/IP/.local 拒否）+ MIME 許可リスト + サイズ上限で受理を判定する。CSP builder は検証後の manifest 値だけを使うため、
  CSP ディレクティブ注入は成立しない。`npx vitest run` が ato-edge 34 件・ato-usercontent-static 35 件成功。
  未検査: 配信スクリプト（deploy）と R2 の実データ、ワークフロー。
### 候補（第3回。コード読解のみ、確定バグに数えない）

- R4: `stop_and_seal`（`seal.rs:104-116`）の 5 段階（active_run → stop_active → evolve_workspace → seal frontier → update_head → release）が
  単一 lock で括られていない。途中で落ちると records だけ進み head が古いまま lease が残り、B6 の回復不能状態に合流する。`stop_and_seal` は冪等でない。
- R5: 起動に失敗した `resume` も `create_branch` を先に行うため branch が残り、同じ `--branch X` の再試行が `already exists` で拒否される。
- H5: attempt journal の UNKNOWN バリアは `work_root` ローカル（`journal.rs:232-275`）。別 Runner / 別 work_root への再配送は止まらない。
  実コード再現で「同じ work root では 7 が 7/8 を止めるが、別 work root では 7 が再び通る」を確認。
- H6: journal の flock はブロッキングで timeout が無く、hosted は 6 slot が 1 `work_root` を共有する。同一 job の 2 attempt が同時に来ると
  slot の heartbeat が止まり得る。
- H7: 失敗メッセージが `{error:#}` の全チェーン（2000 文字上限）で制御プレーンへ送られる。要求 URL が混ざる可能性は未検証。
- H8: 成功した renewal が deadline を後退させ得る（fail-closed、可用性のみ）。
- N2: grant はハンドシェイク callback 内で消費され、その後の `ActiveViewerGuard` / guest・RFB 接続失敗で回復できない（修正済み #3 の残余）。
- N3: origin は signed claims に含まれず、replay cache は jti のみ。allowlist 内の別 origin でも一度だけ通る（権限昇格は無い）。
- N4: replay cache は in-memory / per-gateway。再起動後に replay できる窓は最大 `MAX_ASSERTION_REMAINING_TTL_SECS = 300` 秒。
- I1: `finish_instance_stop` の save が途中失敗すると、`instance.json` は新しい状態へ置換済みなのに ack が出ず、利用者には timeout だけが見える。
- I2: `restored_snapshot()` 単体は metadata-only の証拠（実バイトを読まない）。現行呼び出し元は直後に実バイトを検証する。
- I3: `ReplayMaterializer` の v1 `verify` は `restore` より弱く、`encap` の `verified == target` 検査が恒真になる（通常経路は causal 検査で止まる）。
- I4: `MaterializerRegistry` / `ContractVerifierRegistry` の `register` も insert してから Err（B2 と同型・現行到達不能）。
- I5: static-web の dotfile（`.nojekyll`）は `media_type_for` が `None` を返し producer が bail する（fail closed・可用性）。
- I6: static-web producer の collect→read 間の TOCTOU（collect は symlink を拒否、その後の `fs::metadata` は辿る）。要検証。
- A1: ato-api の ack は `expectedObservationIds.length === 0 ||` で観測集合検査を短絡する。report の `requirement_ids` は zod 上空を許すため、
  空なら任意の satisfied observations で verified になり得る（正規 validator は空を送らない）。
- A2: ack の実行証拠検査は `job.run_id ? … : true` で、run_id の無い job では attempt/lease/realization の一致を検査しない。
- A3: OAuth の `clientAllowsScopes` は client が `apps:read` を持つだけで instance 系スコープを要求できる。Instance アクセスは grant 検査が唯一の防壁。

### 否定（第3回。疑ったが成り立っていた）

- N5: 並行 `TransferBudget::consume` の過剰許可は起きない。`used` は拒否された試行の分も加算する単調量で、安全側の過剰拒否だけが起こる（Lean）。
- N6: DNS は 1 回だけ解決し、deny-any 判定済みの `SocketAddr` に接続する。rebind TOCTOU は無い。
- N7: pixel / terminal の origin 判定は設定時に正規化を強制し、ハンドシェイクは完全一致。
- N8: transaction lock は非再入だが、保持中に lock を取る関数を呼ばない。deadlock は無い。
- N9: `active_run` の復旧は crash に対して吸収的（record が durable である限り）。欠番は `none` で止まり、黙って読み飛ばさない。
- I7: v4 snapshot の restore evidence は index の ref だけに依存しない（bundle 全 payload digest + `observe_run_state` の再 capture、
  ephemeral は `instance_snapshot_missing` で Failed）。
- I8: 同じ bundle の別 import は独立 Instance（application bytes のみ共有）。
- I9: active Run の lease fence（token/status/head lineage）と boot session + start time 照合は健全。
- I10: static-web manifest のパス境界・digest・connect-src・frame-ancestors に新規の受理穴なし。snapshot materializer は verify-only で仕様どおり。
- H9: attempt の一意性（同一 work_root）、execution authorization の単調性・時計非依存、Contract verification failure が
  `realization_failed` に畳み込まれないこと（別経路）を確認。`cargo test -p ato-runtime-attempt --lib journal::` は 7 件成功。
- A4: ato-api の CORS/CSRF 境界は純関数を Node 22 で実行して確認。probe URL は 19 入力で escape 0、
  `canonicalizeOrigin` は path/query/hash/userinfo/`null`/`*` を拒否し、完全一致比較。
- PWA: README の escape-first レンダラと auth 群の既存テスト 178 件が成功（`npx vitest run src/auth src/lib/repoSource`）。
- Desktop: IPC は 7 command に限定、capability は `main` window のみ、navigation は fail-closed の純粋関数で unit test されている（読解）。
## 第4回 セルフホスト実機で見つけた常駐再開の不具合（ato-api、2026-11-20）

専用ゲストで Mail を常駐（`always_on`）させ、ゲストを再起動したところ、API・Caddy・Runner は復帰したが Mail が再開しなかった。
原因は ato-api の 3 箇所で、いずれも修正済み（ato-api `feat/oss-selfhost-node`）。対象は別 repo だが、Runner の復旧報告と対になる判定なのでここでモデル化した。

| ID | 場所（ato-api） | 挙動 | Lean | 修正 commit・回帰テスト |
|---|---|---|---|---|
| A1 | `src/routes/runner_lease_state.ts` `POST /:id/recovery` | Runner が旧 Run の停止を報告しても常駐ポリシーが `running` のまま残る。再調整は `waiting` しか拾わないため再起動されない | `AlwaysOnRecovery.recovery_strands_running_BUG` | `5d8619578`、`runner-lease-state-authz.test.ts`「returns a resident Instance to the restart queue…」 |
| A2 | `src/services/always_on.ts` `scheduleInstanceRestart` | ready になる前に終了した Run の終了報告を重複とみなして捨てる。ポリシーが `starting` のまま残る | `AlwaysOnRecovery.unready_run_end_ignored_BUG` | `2094897c2`、`always-on-policy.test.ts`「requeues a Run that ends before it ever became ready」 |
| A3 | 同 recovery 経路と `src/services/network_authorization.ts` | 停止確認済みの Run の固定 TCP 束縛を解放しない。以後の起動が `fixed_tcp_allocation_busy` で拒否され続ける | `AlwaysOnRecovery.socket_blocks_first_relaunch_BUG`、`held_socket_never_relaunches` | `f662bd5c6`、同テストで束縛が `detached` になること、未確認の停止では `active` のまま残ることを確認 |

A2 と A3 は実機の症状からではなく、A1 のモデルを書くためにコードを読み直した段階と、修正後の再試験で見つけた。A2 は実機では発生させていない（テストでの再現のみ）。

証明した性質:

- `fixed_recovery_relaunches` — 3 つの修正が入った構成では、停止確認済みの復旧報告の次の再調整で必ず新しい Run が起動する（失敗回数によらない）。
- `fixed_recovery_keeps_failures` — ホスト再起動による再開はアプリの失敗回数に数えない。
- `only_the_fixed_combination_is_live` — 3 つの挙動の 8 通りの組み合わせのうち、常に再開するのは修正後の 1 通りだけ。どの修正を 1 つ外しても反例が出るので、仕様が緩すぎないことの確認（mutation）を定理として持つ。[h5i](https://github.com/h5i-dev/h5i) の `h5i app mutate` と同じ狙いを手書きモデルに適用した。h5i の `extract`・`prove` は h5i-app で書いた Rust を Aeneas で Lean に変換する前提で、TypeScript の API と既存の手書きモデルには使っていない。

実機での確認: 修正後の API を配備し、Mail 稼働中にゲストを正常停止 → 起動。操作なしで約 1 分後に Runner が旧 Run の停止を報告し、新しい Run が ready になった。保存済みと未保存の 2 通の下書きを IMAPS で読み戻した。暗号化コールドバックアップを別ディスクイメージへ復元して起動した場合も同じ結果。

限界: バックオフの遅延と、健全期間による失敗回数のリセットは抽象化した（起動の有無ではなく時刻だけを変えるため）。未確認（`unconfirmed`）の復旧は writer の隔離と合わせて別の検証が必要で、ここでは固定 TCP を解放しないことをテストで確認したのみ。複数 Instance が同じ Runner の枠を争う場合と、再調整の同時実行は対象外。

## 第5回 本番の常駐 Instance の書き出しで見つけた不具合（ato-api、2026-11-20）

本番 ato.run で、利用者管理 Runner（`max_slots=1`、明示配置）上の常駐・`runner_volume` の Instance を書き出した際に 4 件が出た。
修正は ato-api `fix/volume-checkpoint-export-races`（base `dev` @ `b948d5fae`）。第4回の A1〜A3 は `feat/oss-selfhost-node` にあり `dev` には未マージで、今回の修正はそれらに依存しない。

| ID | 場所（ato-api） | 挙動 | Lean（修正前 → 修正後） | 修正 commit・回帰テスト |
|---|---|---|---|---|
| V1 | `volume_operations.ts` `advanceVolumeOperation`（待機側） | 停止中の Run は writer を先に返し、lease の終了は後になる。その間の poll が利用者 Runner の枠不足（`user_runner_no_capacity`）を恒久的な失敗 `state_volume_runner_unavailable` にする | `VolumeOperationDispatch.slot_of_stopping_run_fails_operation_BUG` → `fixed_never_fails_on_usable_runner`、`fixed_dispatches_after_stop`、`fixed_retired_runner_still_fails` | `776f73190`、`state-volume-operations.test.ts`「waits while the stopped Run still occupies…」「still fails a waiting personal operation once its Runner is drained」 |
| V2 | 同 `completeVolumeOperation` と `advanceRunning` | Runner の報告が独立した複数の書き込みで、lease を終了させてから operation を確定する。その間の `GET /state/operations/:id` が成功した checkpoint を `interrupted` にし、checkpoint 列だけが進む | `VolumeOperationReport.poll_between_report_writes_interrupts_success_BUG` → `fixed_always_consistent`、`fixed_polls_cannot_interrupt_report`、`fixed_late_report_changes_nothing` | `3fc119c14`、同テスト「records the Runner's success even when an owner poll runs between any two of its writes」「applies nothing from a report that loses…」 |
| V3 | `portable_instance_export.ts` `quiescePortableFilesystemState` | データ付き書き出しが Run を止めて writer の解放を待ち、その後で `state_volume_checkpoint_required` になる | `VolumeExportGate.export_stops_run_then_fails_BUG` → `fixed_refusal_has_no_side_effect`、`fixed_never_stops_a_run`、`fixed_accepts_the_same_exports` | `619dbc1c6`、同テスト「refuses a with-data export of an uncheckpointed volume before stopping its Run」 |
| V4 | `routes/compute_instances.ts` の Stop / Start / `PATCH runtime-policy`、`always_on.ts`、`execution_authorization.ts` | Stop → Start で `desired_state: stopped` のまま Run が動く（更新権限なし・再起動されない）。ポリシーを戻す PATCH と、内容が変わらない PATCH が稼働中の Run を止める | `AlwaysOnOwnerControls.stop_start_is_not_resident_BUG`、`restoring_policy_stops_the_run_BUG`、`unchanged_policy_request_stops_the_run_BUG` → `fixed_stop_start_is_resident`、`fixed_unchanged_policy_is_a_noop`、`fixed_keep_running_adopts_renewable_run`、`fixed_controls_keep_supervision` | `a28aab0ed`、`always-on-start-stop.test.ts`（8 件） |

設計上の決定:

- V3 は「止める前に拒否」を選んだ。Run は writer 取得時に必ず世代を進め、停止は世代を変えないので、止める前に結果が決まる（`stop_does_not_change_the_answer`、`live_run_is_never_exportable`）。受理される書き出しの集合は変わらない。書き出し内で checkpoint まで行う案は、同期的な capture claim に Runner の非同期処理と新しい job 状態（migration）を持ち込むため採らなかった。
- V4 は「明示的な Start は常駐の再開」とした。Run を作る前に `desired_state` を `running` に戻すので、Run は最初から更新可能な権限を持ちポリシーに結び付く。PATCH は、変更がなければ何も止めず、`stopped → running` では更新可能な権限を既に持つ Run だけを引き継ぐ。期限固定で起動した Run は従来どおり止めて置き換える（`fixed_keep_running_replaces_hard_deadline_run`）。

モデルを書く過程で、`supervised` の不変条件が最初の定義では証明できず、「意図が `running` で Run が無いなら再調整待ち」を条件に加えた。

限界: V1 は Runner の heartbeat 切れ（オフライン）をモデル化しておらず、実装でも従来どおり失敗にする。枠を別の Instance の Run が占有し続ける場合、operation は待ち続ける（管理プールの既存挙動と同じ）。V2 は D1 の batch が 1 トランザクションであることを前提にする。V4 は `degraded`・`launching`・`starting` と、訪問による起動（`stopped` のままの on-demand Run）を抽象化した。Start の起動と再調整の同時起動は対象外。本番・staging での実機確認はしていない。

## 引き継ぎ後の修正

対象は `feat/lean-bugcheck` @ `bbeea6f` を起点とする worktree。下の5件を修正し、Lean 4.36.0 で修正版のモデルを検査した。`run.sh` は失敗時に非ゼロ終了し、ソースを書き換えずに回帰検証する。Lean は `$LEAN` または `~/.elan/bin/lean` から指定する。

| # | 状態と検証 |
|---|---|
| 1 | 修正済み。存在しない許可ルートの残りのパスを保持し、`..` を含むルートを拒否。実コードの回帰と8件の path 単体テストを確認。シンボリックリンク変更の競合は未検証。 |
| 2 | 修正済み。有効期限内のグラントを保持し、期限切れを回収。観測した時刻の最大値を保持し、時計が戻っても期限切れグラントを再受理しない。期限内の4096件が埋まった場合は安全側で拒否し続ける。再利用防止は従来どおりゲートウェイのプロセス寿命内に限る。 |
| 3 | 修正済み。サブプロトコルの拒否をグラント消費前に行う。実コードの回帰テスト済み。 |
| 4 | 修正済み。加算を飽和させ、オーバーフローを明示的に拒否。release 相当の再現と netd テスト済み。並行実行の厳密な線形化は Lean では未モデル化。 |
| 5 | 修正済み。空文字と無効な識別子を別のエラーとして返す。Static Web のテスト済み。 |

検証: Lean カーネル検査、`formal/lean-bugcheck/run.sh`、netd ライブラリ129件、Static Web ライブラリ31件、nacelle path 8件。netd のローカルソケットを使うテストはサンドボックス外で実行した。ストレージ容量の数値比較 `StorageCapacity.admission_has_capacity` と境界値も追加で証明したが、容量の観測後に空き領域が変化しないことや予約は保証しない。

`cargo check --workspace --all-targets` と未使用コード走査を更新した。推定は未参照62件、テストのみ104件、未使用の可能性がある依存24件で、コンパイラ警告は0件。以下の説明と数値は元の走査時点の記録であり、現在の一覧は `dead-code/` を参照する。いずれも削除の根拠にはならない。全機能をモデル化しておらず、全バグの解消を意味しない。

## 引き継ぎ時の調査記録（対象コミット 8b058e5）

再実行: リポジトリ直下で `formal/lean-bugcheck/run.sh`（未使用コード走査も含めるなら `RUN_CHECK=1 formal/lean-bugcheck/run.sh`）。同種の検証を続ける手順は `.claude/skills/lean-bugcheck/SKILL.md`。
対象ファイルは `dev` @ 4911151 でも未変更であることを確認済み。
（Lean カーネル検査 → Rust 再現 → 未使用コード走査を一括実行）

## 手法と範囲
- 小さく純粋なセキュリティ判定ロジック 6 箇所（約 1,500 行）を手書きで Lean 4 にモデル化（`AtoBugcheck.lean`、Mathlib 不使用、`sorry` なし、Lean 4.36.0 で検査済み）。
- 「成り立つべき性質」が証明できなかった箇所は反例を定理（`*_BUG`）として固定し、実コードで再現。
- 全体は Rust 約 40 万行。見たのはその一部で、ここに無い箇所は未検証。
- 第2回・第3回でモデルを拡張し、`AtoBugcheck.lean` は 37 セクション・240 定理になった（Semantic Core の正規化/識別子/resolve、authoring と compose の配線ゲート、
  kernel 登録、compose 深さ、CLI resume/lease、data plane の quota/range、record 復旧/seq/分岐 origin、egress の CIDR 形式、hosted lease の寿命/attempt/失敗 code、
  materializer identity、Instance lease、v4 snapshot restore）。
- `run.sh` は Lean カーネル検査の後、未使用コード走査を除く 16 系統の実コード再現（path / budget / netd グラント / static-web / record 連続性 / record 復旧・cursor・分岐 origin / egress CIDR / hosted lease / kernel+compose / kernel depth / authoring gate / Instance lease / portable capsule 3 本 / CLI resume / CLI lifecycle）を順に実行し、失敗時に非ゼロ終了する。

## 引き継ぎ時に見つかった問題（修正前の状態）
| # | 場所 | 内容 | Lean | 実コード再現 | 影響 |
|---|------|------|------|------|------|
| 1 | `extensions/providers/nacelle/src/system/path.rs` `validate_path` | 許可パスが**存在しない**と最も近い既存の祖先に丸められ、許可範囲が広がる。`/x/models` が無ければ `/` 全体が許可される | `PathAllow.widening_BUG` / 修正案 `fixed_sound` | 再現（`/etc/passwd` と `/root/.ssh/id_rsa` が通過） | 関数自体は現状どこからも呼ばれておらず nacelle もワークスペース外。潜在バグ |
| 2 | `services/netd/src/surface_websocket_auth.rs` `authorize_upgrade` | 消費済みグラント集合が 4096 件で満杯になると削除処理が無く、以後そのゲートウェイは正当な新規グラントも全て 500 で拒否 | `Grants.lockout_forever_BUG` | 再現（一時テストを追加して実行後に復元） | pixel / terminal ゲートウェイの恒久的な接続不能。正規の署名付きアサーションが 4096 回必要 |
| 3 | 同上 `on_request` | グラントを消費した**後**にサブプロトコルを検査するため、400 で拒否された接続でもグラントが使用済みになる | 未モデル化（コード読解のみ） | 未実行 | 軽微。#2 の枯渇も早める |
| 4 | `services/netd/src/egress/policy.rs` `TransferBudget::consume` | `fetch_add(bytes) + bytes` が u64 でラップすると予算超過でも `true` | `Budget.wrap_BUG`（非オーバーフロー時は `consume_sound` が成立） | 再現（release ビルド。debug では panic） | 1 回の読み取りは高々数十 KiB なので実質到達不能。`checked_add`/`saturating_add` で解消 |
| 5 | `extensions/materializers/static-web/src/manifest.rs` `validate` | `materialization_id` の文字種・長さ違反が `EmptyField` エラーで返る | 未モデル化 | 未実行 | エラー種別の誤りのみ |

## 証明できた性質（問題なし）
- egress ゲート: IPv4 射影 IPv6（`::ffff:a.b.c.d`）は必ず拒否され、許可された宛先が IPv4 ループバック / 169.254.0.0/16 に到達することはない（`Egress.mapped_always_denied`, `no_loopback_or_linklocal`）。当初ここを疑ったが `::ffff:0:0/96` が拒否リストにあり否定された。
- グラントの再利用は必ず拒否（`Grants.replay_rejected`）。
- capsulehub レート制限: バケット内カウントは上限を超えない（`RateLimit.count_le_limit`）。ただし固定窓なので窓の境界をまたぐと短時間に上限の 2 倍が通る（`boundary_burst`、設計上の性質）。

## 引き継ぎ時の未使用コード候補
`cargo check --workspace --all-targets` は警告 0 件（コンパイラが検出できる未使用は無し。ただし `#[allow(dead_code)]` / `allow(unused*)` が 50 箇所ある）。以下は `dead-code/scan.py` による識別子ベースの推定で、マクロ経由・外部公開 API は誤検出しうる。

1. **コンパイルされないファイル（確認済み）** — `mod` 宣言が無い nacelle の 7 ファイル: `launcher/dev.rs`, `launcher/resolver.rs`, `manager/ebpf_enforcer.rs`, `manager/enforcement_guard.rs`, `manager/startup_gc.rs`, `manager/supervisor_mode.rs`, `tsnet.rs`。
2. **`extensions/providers/nacelle` 全体（Rust 約 2.2 万行）** — ワークスペースの `exclude` にあり、他クレートからの依存も CI ワークフローからの参照も無い。別リポジトリへ移行済みなら削除候補。
3. **存在しないディレクトリを指す `exclude`** — ルート `Cargo.toml` の `extensions/semantics/workspace` と `extensions/adapters/repository` は中身が 0 ファイル。
4. **どこからも参照されない `pub` 項目 63 件** — `dead-code/pub-never-referenced.txt`。例: `services/netd/src/net/client.rs` の `register_ephemeral_ingress` ほか 4 関数、`lib/ipc/src/computation.rs` の `ComputationCommand(Result)`、`lib/runtime-attempt/src/ephemeral.rs` の `launch_controlled`、`apps/cli/src/auth_device.rs` の `load_credential`、nacelle の `verify_license`・`download_*_runtime`。
5. **テストからのみ参照される `pub` 項目 105 件** — `dead-code/pub-test-only.txt`。
6. **未使用の可能性がある依存 24 件** — `dead-code/deps-maybe-unused.txt`。サードパーティ分（`thiserror`×2, `tracing`, `base64`, `data-encoding`, `serde_jcs`, `tempfile`, nacelle の `wasmtime` 系）は確度が高め。`apps/cli` / `apps/formation-worker` の内部 `ato-*` クレート 12 件は lib 名の違いや feature 連動の可能性があり要確認。`cargo-machete` はこの環境でビルドできず未使用。
7. **到達不能な分岐（Lean で証明）** — `manifest.rs` の `file.size > MAX_SAFE_JSON_INTEGER`（`MAX_FILE_SIZE` 検査に包含）と `checked_add(...).ok_or(ClosureLimit)`（オーバーフロー不能）。
8. **その他** — リポジトリ直下にコミットされた `.tmp-consolidated-pr.md` / `.tmp-d-pr.md` / `.tmp-e-pr.md`。`scripts/acceptance/**` の単体 `.rs` 7 本は Cargo 管理外（スクリプトから直接使うものなら問題なし、未確認）。

## 限界
- 第2回以降に確認した範囲と限界は、上記「第2回の限界」を参照する。
- 修正後の再利用防止モデルは期限と既出フラグの判定を抽象化しており、HashMap の実装、同時実行、プロセス再起動、発行者の jti 一意性は証明していない。
- モデルは手書きで、実コードとの対応は読解に依存（シンボリックリンク・並行実行は捨象）。#1 #2 #4 は実コードで再現済み、#3 #5 は読解のみ。
- 仕様の強さを測る mutation testing は未実施（バグ版 / 修正版の対のみ）。
