# RFCs — Technical Specification Documents

All technical specifications are managed as RFCs.

```
rfcs/
├── accepted/      ← accepted specs that back current implementation
├── draft/         ← under discussion, not implementation authority yet
├── archived/      ← retired, old-version, or non-spec documents
├── TEMPLATE_ADR.md
├── TEMPLATE_SPEC.md
└── README.md      ← this file
```

## Document types

| Type | Template | Purpose | Typical size |
|------|-------------|------|------|
| **ADR** | `TEMPLATE_ADR.md` | record a design decision | 1-2 pages; “why this decision” |
| **SPEC** | `TEMPLATE_SPEC.md` | define a component or feature contract | several pages; “how it works” |

## Lifecycle

```
create and discuss in draft/  →  accept  →  move to accepted/
                             →  discard →  move to archived/
accepted/ becomes obsolete   →  move to archived/
```

## Status

| Status | Location | Meaning |
|-----------|------|------|
| `draft` | `draft/` | in design or discussion; not implementation authority |
| `accepted` | `accepted/` | approved and expected to match current implementation |
| `archived` | `archived/` | retired, superseded, or old-version |

## Format rules

### 1. YAML frontmatter (required)

Every RFC starts with frontmatter:

```yaml
---
title: "Document title"
status: draft          # draft | accepted | archived
date: YYYY-MM-DD
author: "@github_handle"
ssot:                  # SPEC only. Source-of-truth code paths
  - "apps/xxx/src/yyy.rs"
related:               # related docs (optional)
  - "CAPSULE_CORE.md"
---
```

### 2. File naming

| Type | Format | Example |
|------|------|-----|
| ADR | `ADR-NNN-kebab-case-title.md` | `ADR-001-runtime-selection-order.md` |
| SPEC | `SCREAMING_SNAKE_SPEC.md` | `CAPSULE_CORE.md`, `NACELLE_SPEC.md` |

- ADRs use a sequential number (`NNN`)
- SPECs are named after the component or contract

### 3. Section structure

**ADR** — Context → Decision → Alternatives Considered → Consequences

**SPEC** — Overview → Scope → Design → Interface → Security → Known limitations → References

Omit sections that do not add value. Numbered sections (for example,
`## 1. Overview`) are recommended.

### 4. Source of Truth (SSOT)

Code is authoritative. Specs explain the code; if they diverge, trust the code.
SPEC docs should declare `ssot` code paths and should cite `file:line` locations
when useful.

### 5. Language

New public-facing docs should be English-first. Existing RFC content is still
mixed-language, and much of the deeper archive remains Japanese for now. Keep a
single document internally consistent.

## Adding a new RFC

1. Copy the template into `draft/`
2. Fill in the frontmatter
3. Review it in a PR
4. When accepted, move it to `accepted/` and update `status: accepted`

## Public site

- GitHub Pages: <https://ato-run.github.io/ato/>
- The RFC index is served from the docs-root docsify site.
- Public navigation should include only `accepted/` and `draft/`. Keep
  `archived/` in the repository, but out of the main public nav.

For local preview:

```bash
cd docs
python3 -m http.server 4173
```

Open <http://localhost:4173> in a browser and select `RFCs`.
