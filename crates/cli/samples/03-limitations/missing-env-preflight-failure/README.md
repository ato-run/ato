# missing-env-preflight-failure

This sample declares `required_env = ["OPENAI_API_KEY"]` and tries to read it at runtime. The intended behavior is for `ato` to block execution pre-launch when the declared env var is absent. In `ato` v0.4.69, however, the auto-provisioner instead generates a synthetic placeholder value — so execution proceeds with a fake key rather than failing. This is a documented gap: `required_env` is currently advisory and not enforced as a hard gate before launch.
