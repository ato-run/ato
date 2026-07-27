# Ready-State acceptance E2E fixture

The guest used to prove, on real hardware, that the interactive-capture
acceptance path verifies the **restored** guest and not the one it was captured
from.

## What the guest serves

| route | purpose |
|---|---|
| `GET /health` | readiness, and the `[seal_at]` probe target |
| `GET /echo-nonce?value=<n>` | echoes `n` verbatim — request-scoped attribution |
| `GET /launch-evidence` | `argv` / `cwd` / `pid` as observed at process start |

## Why a request-scoped nonce, and not one baked into the guest

A nonce built into the image **cannot** distinguish a restored guest from the
guest it was captured from. Restore resumes identical memory, so both answer
identically — that is what restore *means*. Any "the guest returned our secret"
test built that way proves nothing about which VM answered.

Attribution therefore comes from a combination, and the harness asserts all of
it (`hold_phase::fc_kvm_production_hold_release_verify_attributes_the_restored_guest`):

1. **before release** the held guest answers, so the address is live;
2. **after release** the held VMM pid is gone, the slot lock file is gone, and
   the address refuses on five consecutive probes — so nothing is serving there;
3. **after restore** the address answers again and echoes a fresh 128-bit value
   the harness invented *after* the capture was sealed.

Step 2 is what makes step 3 attributable. The nonce's job is only to show the
responder is live and serving *this* request rather than a cache or a proxy.

## Why the launch vector looks strange

```toml
command = ["python3", "server.py", "--label", "boundary probe", "--empty", ""]
```

Both odd arguments are aimed at the comparison, not the app:

- `"boundary probe"` contains a space, so a shell that re-split the vector shows
  up as two arguments instead of one;
- `""` is an **empty argument**, which separates a correct `/proc/self/cmdline`
  decoder — drop exactly one trailing NUL terminator — from one that drops every
  empty piece and therefore cannot tell `["a", "", "b"]` from `["a", "b"]`.

`sys.argv` is recorded as auxiliary evidence only. Python drops the interpreter
from it, so it can never show that `resolved_argv[0]` was honoured; only the
full cmdline vector can.

## Building the guest image

Built through the **existing** v1 recipe producer (`ato build` on a
`schema_version = "1"` manifest) — assemble → export → digest contents → pack.
No bespoke rootfs path exists for this fixture, deliberately: a guest built a
different way would not be evidence about the lane that ships.

```sh
ato build --json                 # emits the v1 receipt incl. guest_image
```

## `[seal_at]` and the hardcoded address

`SealAtConfig` is `command: Vec<String>` with no `{addr}` / `{port}`
substitution, so the command must name the guest address literally. The harness
rewrites that line with the run-unique guest IP before building.

That rewrite is a workaround, and it is exactly why endpoint templating is filed
as a follow-up: with no substitution, a lane that ever gives the restored guest a
different address would leave this command pointing at whatever still answers on
the old one — silently verifying the wrong VM.

## Running

Never use `scripts/ready-state/run-uffd-kvm-smokes.sh` on a live runner host: it
begins each test with `pkill -9 firecracker` and `ip link del fctap0`, which
kills production VMs and deletes the production builder's tap. Use:

```sh
ATO_FC_BIN=/usr/local/bin/firecracker \
ATO_FC_KERNEL=/var/lib/ato/kernel/vmlinux-5.10.223 \
ATO_FC_TEST_ROOTFS=/tmp/atoe2e-rootfs/guest.ext4 \
scripts/e2e/ready-state-acceptance-e2e.sh 10
```

which isolates the tap, work root, IPs and scratch under an `atoe2e` run prefix
and **aborts** rather than reusing or deleting anything it did not create.
