# Stateful Derivation Update: Snapshot, SCE and Atomic Adoption (#1572)

Status: draft. Development Ready only; not Staging Ready.

## Acceptance

```
MigrationAccepted =
    SameK
  ∧ Verified(D_new, K, P)            // Verification Receipt, fully_satisfied
  ∧ SCE.valid                        // state continuity, independent of K
  ∧ EffectPolicy.accepted            // no external side effects during migration
  ∧ AtomicAdoption                   // CAS switch of active D / volume / generation
```

K defines identity. Receipt proves D_new realizes K. SCE proves the data was
carried over. EffectPolicy proves running the migration is safe. Adoption is a
single atomic switch; before it, the candidate volume is not user state.

## Artifacts

`ato.state-snapshot/1`
- object_ref (immutable), format/version
- instance_id / state_slot_id / state_generation
- writer_fence at capture
- integrity (digest, size, format)
- restore_drill (restored into a scratch volume and compared)

`ato.state-continuity-evidence/1` (generic shell; app scenario supplies checks)
- instance/slot/generation, attempt, receipt ref
- checks[] { id, expected, observed, source=app-scenario }
- memos scenario: body equal, count not reduced, id mapping, created_at kept,
  new memo can be created, survives restart

## Order of operations (Copy-on-Migrate)

1. stop and fence the old writer
2. consistent snapshot (app stopped; db + -wal + -shm copied)
3. copy into a candidate volume, run the migration there (old volume untouched)
4. verify K and SCE against the candidate
5. CAS switch active D / volume / generation (fail keeps the old volume)
6. activate D_new; retain snapshot for roll-forward

## Local milestone (memos, single OCI, Linux)

- same K: new bundle root ContractRef equals the old one
- create memo A, snapshot, migrate, verify K + SCE, adopt, restart, assert
  memo A's body is unchanged and a new memo can be created
- negative: corrupted snapshot fails restore_drill; failed K keeps old volume;
  stale writer fence cannot adopt

## Non-goals

Old binary/wire compatibility is not required. Contract K must be preserved and
re-realized. Production deployment is out of scope.
