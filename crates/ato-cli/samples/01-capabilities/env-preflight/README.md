# env-preflight

This sample declares `required_env = ["APP_MESSAGE"]` and prints the variable at runtime. Run it with `APP_MESSAGE=hello capsule ato run .` to see ato pass the env var through to the script. It demonstrates the positive case of `required_env`: when the declared variable is present in the caller's environment, ato propagates it into the capsule process without friction.
