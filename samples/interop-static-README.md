# Interop Static

A minimal static web Capsule used as the interoperability gate for the
contract-root portable application profile. It serves one page and a
`proof.txt` file whose bytes are part of the Contract.

## Run

```sh
ato run capsules/koh0920/interop-static
```

The local realization prints the URL it serves. Add `--no-open` to verify and
exit without opening a browser, and `--verification-receipt receipt.json` to
keep the evidence as canonical JSON.

## What a Run verifies

- `GET /` answers `200`.
- `GET /proof.txt` answers `200` and the body digest matches the Contract
  observation (`sha256:5ec8f5a458f7f500116b8394dd550718915f2d960283905210abe03132ee986a`,
  the same value the CLI receipt records).
- The declared source identity matches the bundled source.

The Run is accepted only when every observation is `satisfied`, so an
accepted Run's receipt is `fully_satisfied: true`.

## License

MPL-2.0
