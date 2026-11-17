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

上表以外の行（portable capsule、runtime-attempt、formation source、data plane、Coordinator/Hosted Runner、ato-api/ato-pwa/Desktop）は第1回の記載から進展なし。

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
- 第2回でモデルを拡張し、`AtoBugcheck.lean` は 20 セクション・104 定理になった（Connection 正規化 / boundary / 識別子、authoring と compose の配線ゲート、kernel 登録、compose 深さ、CLI resume と lease、data plane の quota と range、resolve の信頼境界）。
- `run.sh` は Lean カーネル検査の後、未使用コード走査を除く 9 系統の実コード再現（path / budget / netd / static-web / record / kernel+compose / kernel depth / authoring gate / CLI lifecycle）を順に実行し、失敗時に非ゼロ終了する。

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
