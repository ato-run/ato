# Windows attempt journal修正後のnative CI

測定ID `formation-windows-attempt-journal-native-20261003-02`。Actions [37113580365](https://github.com/ato-run/ato/actions/runs/37113580365)、実行Source pin `384457c97225c3bb196d3974cb84e9a8eaecabff`。workflow `computation CLI` はUbuntu・macOS・Windowsの3jobとも成功した。

Windows job `111175979866` はstable Rust 1.99.0 (`b940084d7`)、MSVC targetで実行した。journalのcode blobは `f0e022c4ad4448e8f9352f0ef4eed068706841cd` で、ローカル修正commit `478ab485` と一致する。この結果を後の文書commitへ付け替えない。

| Windows step | native結果 |
|---|---|
| 6: `Windows durable attempt journal` | **8 PASS / 0 FAIL**。readonly destinationのpublication拒否と旧reservation保持、Unicode rootでの予約・開始・終了の置換/reopen、request lock contention、UNKNOWN/redelivery、既存の履歴・書込み失敗を含む。 |
| 8: 通常CLI/Adapter/Kernel/Objects tests | 成功。`tests/portable_application.rs` は **15 PASS / 0 FAIL**。以前開始前に失敗したprocess/static fixture、停止・再起動・snapshotの各fixtureも通過した。 |
| 9: `cargo clippy -p ato-cli --all-targets -- -D warnings` | 成功。 |

以前のhead `5798e23d` / run [37109321016](https://github.com/ato-run/ato/actions/runs/37109321016) のWindows portable 7 PASS / 8 FAILは、その時点の失敗として保持する。[先行ローカル記録](formation-windows-attempt-journal-local-20261003.md)も変更していない。新stepが成功したことと、修正前の失敗がなかったことを同一視しない。

watchは60秒間隔で実行し、run終了後にmetadataとWindows job logを各1回取得した。固定SHA、step、8件・15件の出力行、job一覧、取得したファイルのhashを [機械記録](evidence/formation-windows-attempt-journal-native-20261003.json) に保存した。

この結果はcontrolled process/static fixtureのnative CIである。電源断耐久性の実験、実Codex/Claude Code、PWA入力resume、今回のKutt/changedetection artifactによる機能・保存・再起動は含まれない。実受入のRuntime attempt数へCI fixtureを加算せず、この記録ではCI内のattempt総数も集計していない。独立有料推論API call、配備、remote migration、flag変更、既存Source/K/D・receipt・台帳の修正はない。
