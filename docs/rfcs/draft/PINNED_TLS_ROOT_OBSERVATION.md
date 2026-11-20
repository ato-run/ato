# 組織内CAに固定したTLS観測

Status: Draft

`ato.contract.tls-pinned-root@1` は、論理Portで提示される証明書を、Kに指定した単一の信頼アンカーで検証する。著者は `server_name`、`start`、`root_ca_digest` を指定する。`root_ca_digest` は公開CA証明書のDER全体を対象にした小文字の `sha256:<64 hex>` である。

```toml
[[contract.observation]]
id = "imaps-certificate"
use = "ato.contract.tls-pinned-root@1"
port = "stalwart.imaps"
server_name = "mail.example.test"
start = "implicit"
root_ca_digest = "sha256:<公開CA DERの64桁小文字hex>"
```

FormationはfingerprintをBoundRequirementの `digest` に格納する。公開root用の `ato.contract.tls@1` は引き続きdigestの指定を拒否し、Mozillaの公開root集合を使う。CAを切り替える場合は、新しい観測を含むKをsealする。leaf証明書の更新時は、同じCA、名前、期限の要件で再検証する。

CLIとhosted validatorは `ATO_TLS_ROOT_DIR/<64 hex>.der` から最大64 KiBの証明書を読み、DERのSHA-256を照合して単一の信頼アンカーとして使用する。このディレクトリには運用者が検証済みの公開証明書を配置し、ワークロードには書き込み権を与えない。配布先のファイルが欠落・破損している場合、観測は成功しない。API用CA・システムのCA・公開root集合へのfallbackは行わない。運用者は各観測のrootごとに対象となる名前と用途を審査する。

既存のTLS観測と同じ実ハンドシェイク、名前、証明書の有効期間、chain検証を行う。`implicit` と `smtp-starttls` を同じprobeで扱い、成功時はreceiptの `evidence.tls.trust_anchor` に実際に使用したDERのfingerprintを記録する。Kとの照合結果、TLSの完了、選択したattemptなど既存のreceipt検証を引き継ぐ。古いreaderは未知のverifierを拒否する。

実メールの配送、アプリの保存、同一URLでの再開は別の観測で確かめる。
