# Ato Docs

The public surface of this directory is **topic-first, with roles separated inside
each page**. Instead of forcing `wiki / design / spec` as top-level directories,
each topic page should contain these four sections.

1. Overview
2. How it works
3. Specification
4. Design Notes

## Topics

- [Run](run.md)
- [Capsule](capsule.md)
- [Sandbox](sandbox.md)
- [Execution Identity](execution-identity.md)

## Reference

- [Core Architecture](core-architecture.md)
- [Glossary](glossary-reference.md)
- [RFCs](rfcs/README.md)
- [Topic Page Template](topic-page-template.md)

## Internal docs

Plans, research notes, handoffs, and dashboards belong under
[`internal/`](internal/README.md). They are workspace artifacts, not part of the
main public navigation.

## Source of truth

Code is the source of truth. These topic pages should track the current
implementation in `crates/`, while RFCs remain the deeper contract and design
history.
