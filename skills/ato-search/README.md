# ato-search

Agent Skill for discovering apps and tools through the ato.run search API.

Copy this `skills/ato-search` directory into a compatible host’s skill directory, or use `SKILL.md` and `endpoints.md` directly. The API vocabulary can be checked at `GET /v1/search/vocabulary`.

The skill was migrated from [ato-run/ato-search-skill](https://github.com/ato-run/ato-search-skill) at commit `bb11d8b0e97d9bd05cdaff2f1cf3e009ea7b3e94`. The legacy repository remains available for existing links. The endpoint and caching notes were updated against the current API source; deployment behavior has not been independently verified by this migration.

`examples.jsonl` contains curated examples. Update the skill and examples when the API capability vocabulary changes. The imported files are MIT licensed; see `LICENSE`.
