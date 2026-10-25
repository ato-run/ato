#!/usr/bin/env python3
"""6b-D0 ledger generator. Runtime fields are computed from a typed runtime kind
and the typed current catalog, never from free text. Fails on inconsistency."""
import json, re, sys, collections
from pathlib import Path

CATALOG = {"node": ["20.20.2", "22.14.0"], "python": ["3.11.11", "3.12.7", "3.13.1"]}
DEFAULT = {"node": "20.20.2", "python": "3.12.7"}  # Ato defaults when a source declares nothing
FAM = {"A": "Node service", "B": "Python service", "C": "Go", "D": "PHP", "E": "Rust", "F": "Ruby",
       "G": "JVM", "H": ".NET", "I": "multi-service"}


def vt(v):
    return tuple(int(x) for x in (v.split(".") + ["0", "0"])[:3])


def satisfies(version, requirement):
    """Tiny constraint evaluator for the forms present in these sources."""
    v = vt(version)
    for clause in re.split(r"[,\s]+(?=[<>=^~])", requirement.strip()):
        clause = clause.strip()
        if not clause:
            continue
        m = re.match(r"^(>=|<=|>|<|==|\^|~)?\s*v?([0-9][0-9.]*)$", clause)
        if not m:
            raise ValueError(f"unparsed requirement clause {clause!r}")
        op, raw = m.group(1) or "exact", m.group(2)
        r = vt(raw)
        parts = raw.count(".") + 1
        ok = {">=": v >= r, ">": v > r, "<=": v <= r, "<": v < r, "==": v == r,
              "^": v >= r and v[0] == r[0], "~": v >= r and v[:2] == r[:2],
              "exact": v[:parts] == r[:parts]}[op]
        if not ok:
            return False
    return True


def runtime_fields(kind, requirement):
    if kind not in CATALOG:
        return False, False
    if requirement is None:
        return True, False  # provisionable via the Ato default; compatibility undeclared
    ok = any(satisfies(v, requirement) for v in CATALOG[kind])
    return ok, ok


def main():
    rows = json.loads(Path(sys.argv[1]).read_text())
    facts = json.loads(Path(sys.argv[2]).read_text())
    apps = []
    for r in rows:
        in_catalog, declared_ok = runtime_fields(r["primary_runtime_kind"], r["runtime_requirement"])
        r = dict(r, runtime_in_current_catalog=in_catalog, runtime_declared_and_satisfiable=declared_ok,
                 runtime_uses_ato_default=in_catalog and not declared_ok,
                 port_provable_natively=False, source_facts=facts[str(r["index"])],
                 install_requirement="dependency resolution (network) for every family; no offline cache preregistered")
        # Consistency: a kind outside the catalog can never be provisionable.
        assert r["primary_runtime_kind"] in CATALOG or not in_catalog, r["name"]
        if r["family"] in "CDEFGH":
            assert not in_catalog, r["name"]
        apps.append(r)
    cands = [a for a in apps if a["family"] in "AB" and a["typed_native_launch"]
             and a["runtime_in_current_catalog"] and not a["external_service_required"]]
    fam = collections.Counter(a["family_name"] for a in apps)
    kinds = collections.Counter(a["primary_runtime_kind"] for a in apps)
    agg = {"typed_native_launch": sum(a["typed_native_launch"] for a in apps),
           "runtime_in_current_catalog": sum(a["runtime_in_current_catalog"] for a in apps),
           "runtime_declared_and_satisfiable": sum(a["runtime_declared_and_satisfiable"] for a in apps),
           "runtime_uses_ato_default": sum(a["runtime_uses_ato_default"] for a in apps),
           "dependency_locked": sum(a["dependency_locked"] for a in apps),
           "external_service_required": sum(a["external_service_required"] for a in apps),
           "port_provable_natively": 0,
           "primary_runtime_kinds": dict(kinds.most_common()),
           "outside_catalog_by_kind": {k: v for k, v in kinds.items() if k not in CATALOG},
           "catalog_kind_but_version_unsatisfiable": sorted(a["name"] for a in apps if a["primary_runtime_kind"] in CATALOG and not a["runtime_in_current_catalog"])}
    assert agg["runtime_in_current_catalog"] + sum(agg["outside_catalog_by_kind"].values()) + len(agg["catalog_kind_but_version_unsatisfiable"]) == len(apps)
    ledger = {"schema": "ato.formation-service-qualification-d0/2", "wave": "6b-D0", "base": "461fa78cd871ccf376267b1115dcf46a54b13019",
              "input": "the 41 non-success sources with a preregistered service/process shape in formation-coverage-50 (same pinned archives; nothing re-fetched)",
              "method": "static reading of root manifests, lockfiles, version files, Dockerfiles, compose files and declared Python constraints (setup.py python_requires, poetry python); no execution, install or model call",
              "runtime_catalog": CATALOG, "ato_defaults_when_undeclared": DEFAULT,
              "runtime_metric_definition": "runtime_in_current_catalog = the primary execution runtime kind is provisionable by the current Ato catalog and the source's declared version requirement (if any) is satisfiable by a catalog version; an undeclared requirement uses the Ato default and is counted separately as runtime_uses_ato_default. A readable or exact version of an uncatalogued runtime (Go/PHP/Rust/Ruby/JVM/.NET) is never true. For multi-service apps the main application process runtime is used; external services stay a separate field.",
              "family_distribution": dict(fam.most_common()), "aggregate": agg,
              "first_candidates": [{"name": a["name"], "family": a["family_name"], "launch": a["deterministic_launch_declaration"],
                                    "runtime": f"{a['primary_runtime_kind']} {a['runtime_requirement'] or 'undeclared (Ato default)'}",
                                    "lock": a["dependency_lock"], "port": a["port_declaration"], "state": a["persistent_state"]} for a in cands],
              "candidate_remaining_blockers": json.loads(Path(sys.argv[3]).read_text()),
              "guess_prohibitions": ["no port inferred from framework/start script/README",
                                     "no argv from main.py, cmd/, framework name or README shell",
                                     "Dockerfile CMD/EXPOSE is container (OCI Adapter) metadata, not a native launch/port declaration"],
              "correction": {"previous_schema": "ato.formation-service-qualification-d0/1 (d7375c68)",
                             "error": "runtime_in_current_catalog was derived from free text and was true for 17 uncatalogued Go/PHP/Rust/Ruby/JVM/.NET sources (27/41 claimed)",
                             "extraction_gap_fixed": "declared Python constraints in setup.py/poetry (SearXNG, changedetection.io, PdfDing, Shynet) were previously recorded as unpinned"},
              "model_calls": 0, "deploy": "none", "remote_migration": "none", "applications": sorted(apps, key=lambda a: a["index"])}
    Path(sys.argv[4]).write_text(json.dumps(ledger, indent=2) + "\n")
    print(json.dumps({"aggregate": agg, "candidates": [c["name"] for c in ledger["first_candidates"]]}, indent=1))


if __name__ == "__main__":
    main()
