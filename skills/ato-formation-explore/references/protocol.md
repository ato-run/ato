# Sessionの操作と型付き契約

Producerは次のCLIだけでBridgeに接続する。`CONNECTION`は所有者から提供された接続ファイルのパス。内容を表示しない。

```sh
ato form-session --connection "$CONNECTION" status
ato form-session --connection "$CONNECTION" next
ato form-session --connection "$CONNECTION" submit \
  --exchange-id "$EXCHANGE_ID" --input-sha256 "$INPUT_SHA256"
```

submitのstdinへProducer自身が作った応答JSONを渡す。人にJSONの転記を依頼する必要はなく、応答ファイルの作成も必須ではない。入力digestを再計算したり、別のexchangeへ付け替えたりしない。保存済み入力の取得は枠を消費しない。submitの再送は保存済みの同一内容に限る。異なる内容、古い入力、別exchangeへの提出は拒否されるため、新しい応答で上書きしない。

取消を明示的に依頼された場合は同じBridgeへ送る:

```sh
ato form-session --connection "$CONNECTION" cancel
```

cancel後も保存済み状態を照合する。`cleanup: not_confirmed`をcleanup済みへ読み替えない。

## Status/nextのview

両操作は`ato.formation-session-view/1`を返す。`search_id`、`configuration_ref`、Searchの`deadline_ms`、`exchanges_used`/`exchanges_remaining`、`connected`、`waiting_reason`、`next_operation`を確認する。現在exchangeの`exchange_deadline_ms`がある場合は、Search deadlineより早いこの期限にも従う。再接続でも保存済みのexchange期限を延長しない。`internal_LLM_calls`/`token_usage`/`cost`は取得不能なら`unknown`。

`exchanges_used`は保存済み入力を数え、未回答の現在exchangeも含む。`exchanges_remaining: 0`は新しいexchangeを追加できないという意味であり、`next_operation: submit`かつ`input`がある現在exchangeには応答できる。最後に割り当て済みの入力を枠切れと誤認して放棄せず、Bridgeの次の操作と期限に従う。

`progress`はSearch状態、`pause_reason`/`termination_reason`、ContractRef、Search budget、attempt一覧、`unresolved_attempts`、approval/deployed/cleanup状態を示す。`exchange`がある場合はその`exchange_id`/`input_sha256`/`response_saved`を使う。保存済み応答を処理中、deadline、UNKNOWN、`needs_input`では`input: null`となるため、以前の入力への新しい応答を作らない。

Ownerが終了結果を照合すると、`progress`には実行pin、消費round、receiptのContractRef/DerivationRef/attempt ID/digest/`fully_satisfied`などの公開summaryが追加される。これはreceipt本文や観測bodyをProducerへ公開するものではない。ACKはそのsummaryだけで確認できないため、`unknown`を完了済みに読み替えずOwnerの証拠と照合する。

Bridge切断後は接続ファイルのextensionを`status.json`へ置き換えた公開viewがCLIのfallbackになる。`connected: false`の保存済みviewを現在のRuntime状態として扱わず、最終既知状態として報告する。接続ファイルの直接表示やowner journalの読取へ切り替えない。

## 共通入力

入力schemaは`ato.formation-reasoning-input/1`。固定Source identity、`frozen_contract_ref`、検査済みinventory/context、operation catalog、Runtime能力、lowering capabilities、過去のDと失敗証拠、残枠が含まれる。`available_variables`は変数のメタデータであり、値ではない。回答可能な`next`には、固定したprompt versionの共通本文が`instructions`として付く。この本文と`input`を合わせて読み、別versionのpromptで補わない。

Bridgeから返された待機理由と次の操作を優先する。API側のcall枠とSession exchange枠の台帳を区別し、読み直し・接続し直し・入力待ちで補充しない。prompt versionごとの能力差は入力のcatalogで判断する。

## 応答

submitには`ato.formation-proposal/1`のbatchを渡す。Session envelope、proposal ID、DerivationRef、成功receiptはAtoが生成する。JSONだけを渡し、Markdownや説明文を外側へ付けない。

| `kind` | 内容と根拠 |
|---|---|
| `inspect_source` | inventoryに載った`file_id`と`digest`の参照。新たな根拠が必要なSourceのみを取得する。 |
| `propose_derivation` | catalogに登録されたoperationと、取得済みSource/失敗証拠によるbasis。未登録operationやSourceは作らない。 |
| `modify_derivation` | 入力で許可された`base_derivation_ref`とoperations。既知Dなしの開始時には使えない。 |
| `unsupported` | 入力契約が許す理由と、公開可能なbounded evidence。`needs_input`・`no_progress`・能力不足を区別する。 |

アプリに依存しない停止応答の例:

```json
{"schema":"ato.formation-proposal/1","proposals":[{"kind":"unsupported","reason":"no_progress"}]}
```

許可されたmember数、Source参照数、byte上限とfield制約は共通入力が定める。Rust validatorが唯一のvalidation/compile authorityであり、Skillや補助scriptに並行validatorを作らない。検証拒否は具体的なfeedbackを使って修正する。失敗したbuildだけで`source_broken`と断定しない。

## 所有者とProducerの分担

所有者は承認済みplan、Ato認証、入力権限、Requester、Coordinator、Runtimeを管理する。Producerは限定Bridgeから取得した入力と応答だけを扱う。Searchを新規作成する際はplanの事前承認が必要。既存Searchへ接続する際は未終了であり、元のSource/K/期限/消費枠が維持されていることを確認する。

Ownerは既存の`ato form`のSource/K/Runtime/探索config引数に`--session-bridge CONNECTION_FILE`を追加する。再接続は元の`--search-id`と同じRequester journalを指定したformを使い、deadlineや枠を初期化しない。ProducerはこのOwnerコマンドを代行せず、準備済みBridgeへ接続する。

Bridge接続で所有者のcredential directoryへアクセス権を増やさない。Ownerの停止・ACK・予約解放・cleanupが不明なら、ProducerからホストのprocessやDBを直接操作せず、その項目を引き継ぐ。
