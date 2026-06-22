# missing-required-field

This sample has a valid TOML `capsule.toml` that omits the required `name` field. Running `ato run .` fails at manifest schema validation with "missing field `name`" before any provisioning or execution occurs. It demonstrates ato's strict schema enforcement: syntactically valid TOML is still rejected if required fields are absent.
