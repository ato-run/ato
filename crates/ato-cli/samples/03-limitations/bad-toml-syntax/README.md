# bad-toml-syntax

This sample contains a deliberately broken `capsule.toml` with an unclosed TOML table header (`[meta` with no closing bracket). Running `ato run .` surfaces a clear TOML parse error at line 1 before any execution occurs. It demonstrates ato's fail-closed manifest validation: a malformed manifest is never silently ignored.
