# OAuth 遷移実装の振り返りと今後の方針

## 文書の目的

この文書は、ato-desktop でこれまで試してきた OAuth 遷移実装がなぜ安定しなかったのかを整理し、今後どの方向で実装を進めるべきかをチーム内で議論しやすくするための説明資料です。

対象は ato-desktop のホスト実装を触る開発者です。主に次の 3 点を扱います。

- これまでうまくいかなかった事例
- それぞれに対して考えられる原因
- これから採るべき実装方針

この文書は OAuth 全般を扱いますが、特に Google OAuth、popup ベースの認証、passkey、macOS Passwords の扱いに重点を置きます。

## 現在の仕様

### 外部サイトの扱い

ato-desktop は外部サイトを ExternalUrl として扱います。外部サイトは埋め込み Wry WebView で表示されますが、guest capsule 向けの preload bridge や IPC は無効です。

根拠:

- GuestRoute::ExternalUrl は外部 URL 用のルートである: src/state/mod.rs
- 外部 URL への遷移時は CapabilityGrant::OpenExternal のみが付与される: src/state/mod.rs
- 外部 URL では inject_bridge=false, enable_ipc=false, enable_custom_protocol=false: src/webview.rs

実装上の意味は次の通りです。

- 外部サイトは一般のウェブページとして表示する
- ホストアプリがページ内部の認証 API を積極的に仲介しない
- 認証をアプリ内で完結させるためのネイティブ権限やドメイン結合も持たない

### 外部ブラウザへの escape hatch

ホスト側には shell.open があり、URL を macOS の open コマンドで既定ブラウザへ渡せます。

根拠:

- shell.open を open_external に dispatch している: src/bridge.rs
- open_external は open <url> を実行する: src/bridge.rs

これは今後の実装で使える最も安全な escape hatch です。

### popup の現在状態

現時点では macOS の Wry popup が生成されるよう、new-window handler を Allow で有効化しています。

根拠:

- with_new_window_req_handler(|_, _| NewWindowResponse::Allow) が設定されている: src/webview.rs

したがって、現在の主問題は popup が絶対に出ないことではなく、popup が出ても OAuth が安定しないことや、passkey / Passwords のような OS・ブラウザ依存の認証が埋め込み WebView で完結しないことです。

## これまでうまくいかなかった事例

### 事例 1: Canva などで Google OAuth を押しても別タブ・別ウィンドウが開かなかった

#### 何を期待していたか

window.open や _blank 遷移により、Google OAuth 用の popup が別ウィンドウで開くことを期待していました。

#### 実際に起きたこと

認証ボタンを押しても何も表示されず、ログインフローに進めませんでした。

#### 考えられる原因

最終的に分かった直接原因は、macOS の Wry では new-window handler が未設定だと popup 要求を生成しないことでした。

つまり当初の実装では:

- サイト側は popup を要求していた
- しかしホスト側が popup 作成を許可していなかった
- 結果として OAuth フローが見えないまま失敗した

#### 学び

OAuth の popup 問題を調べるときに、まずサイト固有ロジックを疑うのではなく、WebView ランタイムが window.open をどう扱うかを先に確認するべきでした。

### 事例 2: OAuth を既定ブラウザで開く方向に切り替えたが、Chrome に遷移しなかった

#### 何を期待していたか

Google OAuth のような認証だけは埋め込み WebView から切り出し、既定ブラウザで開くことを期待していました。

#### 実際に起きたこと

一部のケースではアプリ内ウィンドウが開くか、あるいは何も起きず、期待した Chrome 遷移になりませんでした。

#### そのとき行った試み

以下の interception を段階的に追加して試しました。

- new-window request の捕捉
- window.open の JS interception
- target=_blank のリンク interception
- form submit / requestSubmit の interception
- iframe relay 的な検知
- shell.open による既定ブラウザ起動
- 詳細ログの追加

#### 考えられる原因

原因は 1 つではなく、次の複合でした。

1. サイトによっては OAuth URL が単純な anchor や form action に現れない
2. Canva では Google Identity Services や FedCM に近い挙動があり、最終的な Google URL をホスト側で素直に観測できなかった
3. 埋め込み WebView から見えるのは about:blank だけで、その先の実 URL が取れないケースがあった
4. ページ側の実装がブラウザ前提で、ホスト側の URL heuristic が追いつかなかった

#### 学び

認証っぽい URL を見つけたら外部ブラウザに逃がすという heuristic は、単純な OAuth には効いても、GIS や FedCM を使う現代的なフローには安定しません。

### 事例 3: fallback が誤った URL を開き、ato.run や Canva 本体へ遷移した

#### 何を期待していたか

Google OAuth の URL が取れない場合でも、ユーザーが直前に操作した現在ページを外部ブラウザへ渡せばログイン継続できる可能性を期待していました。

#### 実際に起きたこと

最初は stale な URL を拾って ato.run を開き、その修正後も Canva 自体の URL を開いてしまい、Google 認証には進みませんでした。

#### 考えられる原因

1. fallback の前提が弱かった
2. 現在ページ = 認証先ページではない
3. OAuth が JS 内部状態から始まる場合、トップレベル URL は認証の入り口ではない
4. stale state を握ると、まったく関係ない URL を開きうる

#### 学び

認証 URL が観測できない場合に現在ページを開く fallback は、ユーザー体験の改善ではなく誤動作を増やしやすいです。特に OAuth では、現在ページ fallback は原則として採らない方がよいです。

### 事例 4: 実装を元の popover 方式へ戻した後も OAuth ウィンドウが表示されなかった

#### 何を期待していたか

OAuth 専用の外部ブラウザ実装を全部戻せば、元の in-app popup 方式に復帰することを期待していました。

#### 実際に起きたこと

rollback 後も popup は表示されませんでした。

#### 考えられる原因

rollback 前に一度回避ロジックを足していたため、元実装に戻しただけでは macOS Wry の popup 要件を満たしていなかったことが原因です。

直接原因は事例 1 と同じで、new-window handler が無い限り popup が生成されませんでした。

#### 学び

元に戻すだけでは、もともと存在していたランタイム制約まで解消できないことがあります。rollback 後にも低レベルの前提条件を再検証する必要があります。

### 事例 5: passkey がエラーになり、macOS Passwords も埋め込み WebView では期待通りに扱えない

#### 何を期待していたか

外部サイトの認証で、Safari や Chrome と同様に passkey や macOS Passwords が自然に使えることを期待していました。

#### 実際に起きたこと

passkey はエラーになり、OS の資格情報ストアとブラウザが持つ前提に届いていない挙動になりました。

#### 考えられる原因

これは OAuth popup とは別系統の問題です。主な原因候補は次の通りです。

1. passkey は relying party に強く結びつく
2. macOS Passwords / shared credential 系は associated domains や webcredentials といった app 側の entitlement を前提にする
3. ato-desktop にはそれらの entitlement と AuthenticationServices 統合が無い
4. 一般の第三者サイトを埋め込み WKWebView で開くだけでは、ブラウザ同等の credential UX にはならない

#### 学び

OAuth popup と passkey は一緒に見えますが、実際には別問題です。

- OAuth popup は windowing と URL 遷移の問題
- passkey / Passwords はブラウザ文脈、OS 権限、relying party 境界の問題

後者は heuristic や JS interception では解決しません。

## 失敗パターンの共通原因

これまでの事例をまとめると、共通して次の構造的な問題がありました。

### 1. 埋め込み WebView を汎用ブラウザとして扱おうとした

ato-desktop はホスト shell であって、Safari や Chrome のようなフルブラウザではありません。にもかかわらず、第三者サイトの認証フロー、popup、passkey、OS credential 連携をすべて同じ文脈で扱おうとしたため、ランタイム制約に繰り返しぶつかりました。

### 2. URL が観測できれば制御できると考えすぎた

近年の OAuth 実装は単純な redirect URL だけではなく、JS SDK、GIS、FedCM、popup mediator を経由します。ホスト側から見える URL だけで認証を制御するのは限界があります。

### 3. 第三者サイト認証と第一者認証を分けて設計していなかった

Google、Canva、GitHub のような第三者サイトと、将来的な ato.run 自身の認証は、本来別の設計にするべきでした。ここを分けないと、第三者サイトのために過剰なネイティブ実装を考えるか、逆に第一者認証の改善機会を逃します。

### 4. rollback と forward fix の境界が曖昧だった

一度外部ブラウザ方針へ寄せてから元に戻したため、どこまでを戻すか、何は基盤として残すかが曖昧になりやすかったです。結果として、popup 生成のような必要最小限の前提まで一緒に消えてしまいました。

## 今後どう実装するか

### 基本方針

今後は次の 2 層に分けて設計するのが妥当です。

#### 方針 A: 第三者サイトの認証はブラウザへ委譲する

Google OAuth、Canva、GitHub など、ato が所有していないドメインの認証は、原則として既定ブラウザへ出すべきです。

理由:

- popup 制御をホスト側で再現しきれない
- passkey / macOS Passwords はブラウザや OS 権限に依存する
- 第三者サイトごとの特殊フローに埋め込み WebView で追従するコストが高い
- 既定ブラウザならユーザーの既存セッションと credential UX をそのまま使える

#### 方針 B: 第一者認証だけは別トラックでネイティブ統合を検討する

将来 ato.run 自身の認証体験を強くしたいなら、associated domains、webcredentials、AuthenticationServices を用いた第一者向け実装を別途検討します。

理由:

- relying party を自社ドメインに限定できる
- entitlement とドメイン管理を自分たちで持てる
- passkey / Passwords を製品体験として最適化しやすい

ただしこれは、任意サイトの OAuth をアプリ内で安定させる話とは切り分ける必要があります。

### 推奨アーキテクチャ

短中期では、次のハイブリッド方式を推奨します。

#### 1. AuthPolicy を導入する

各 route に対して、認証時の扱いを明示します。例えば次の分類です。

- InAppAllowed: 埋め込みのまま扱う
- BrowserPreferred: 認証が始まったら外部ブラウザへ委譲する
- BrowserRequired: 最初から外部ブラウザで開く
- FirstPartyNative: 第一者認証を将来ネイティブ実装する余地を持つ

最初はシンプルに、外部 URL 全体を BrowserPreferred にしてもよいです。

#### 2. 認証検知を heuristic ではなく policy に寄せる

Google URL らしい、popup らしいという判定を増やすのではなく、次のいずれかが起きたらブラウザに出す設計に寄せます。

- popup 要求が発生した
- 認証プロバイダのドメインへ遷移した
- WebAuthn / passkey エラーが発生した
- サービス単位で最初からブラウザ委譲する設定になっている

つまり URL 抽出ロジックを賢くするより、認証フローをホストの policy で切り替える方が安定します。

#### 3. ブラウザ復帰経路を別途設計する

ブラウザ委譲を正規仕様にするなら、認証後にどうアプリへ戻るかを決める必要があります。候補は次の通りです。

- custom URL scheme
- localhost loopback callback
- ato-cli / guest backend を経由した session handoff

この復帰経路が無いと、認証成功後の UX が途切れます。

#### 4. popup の in-app 対応は維持するが、成功前提にはしない

with_new_window_req_handler(...Allow) は残して問題ありません。これは一般サイトの新規ウィンドウや簡易 popup に必要です。

ただし、OAuth や passkey がこれだけで安定する前提は捨てるべきです。

## 非推奨の方向性

次の方向は、現時点では採らない方がよいです。

### 1. 認証 URL heuristic をさらに積み増す

理由:

- サイト依存が強い
- GIS / FedCM に追従しにくい
- stale URL やトップページ fallback の誤動作を再発しやすい

### 2. 現在ページ fallback を復活させる

理由:

- 認証先 URL でないページを誤って開く
- 失敗時の UX が改善しない
- 原因究明を難しくする

### 3. 第三者サイトの passkey をホスト側で汎用対応しようとする

理由:

- ブラウザと OS 権限の責務を再実装することになる
- app entitlement と relying party 境界の問題を解けない
- 製品の主責務に対してコストが大きすぎる

## 推奨する次アクション

優先度順に並べると、次の順で進めるのがよいです。

1. 第三者認証はブラウザ委譲を標準仕様とする
2. AuthPolicy を state に持たせ、外部 URL の認証方針を route 単位で制御できるようにする
3. ブラウザ委譲後の復帰経路を設計する
4. passkey / macOS Passwords は第一者認証トラックとして別途設計する
5. 現在の popup 許可は維持しつつ、認証成功の中核には据えない

## 最終結論

これまでの OAuth 実装がうまくいかなかった主因は、ato-desktop の埋め込み WebViewを汎用ブラウザとして扱い、第三者サイトの popup OAuth と passkey / Passwords を同じ延長で解こうとしたことです。

今後は次の整理に切り替えるべきです。

- 第三者サイトの OAuth、passkey、macOS Passwords は既定ブラウザへ委譲する
- in-app popup は補助的に扱う
- ato.run のような第一者認証だけは、必要になった時点でネイティブ統合を検討する

この方針にすると、これまで繰り返し失敗してきた URL heuristic 依存の OAuth 制御から抜けられ、実装の責務境界も明確になります。
