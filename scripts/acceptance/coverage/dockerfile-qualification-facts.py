#!/usr/bin/env python3
"""6b-D1 facts: parse every Dockerfile of the pinned archives. Static only:
no build, no pull, no network. Output is evidence, not a build decision."""
import json, re, sys, tarfile

NET = re.compile(r"\b(apt-get|apt|apk|yum|dnf|microdnf|pip3?|uv|poetry|npm|npx|yarn|pnpm|corepack|bun|go\s+(build|mod|install|get)|cargo|rustup|composer|bundle|gem|dotnet|mvn|gradle|curl|wget|git\s+clone)\b")
NAME = re.compile(r"(^|/)(Dockerfile|Containerfile)([._-][^/]*)?$|(^|/)[^/]+\.(dockerfile|Dockerfile)$")


def logical_lines(text):
    out, cur = [], ""
    for raw in text.splitlines():
        line = raw.strip()
        if not cur and (not line or line.startswith("#")):
            continue
        if line.endswith("\\"):
            cur += line[:-1] + " "
            continue
        cur += line
        out.append(cur.strip())
        cur = ""
    if cur:
        out.append(cur.strip())
    return out


def parse(text):
    stages, cur = [], None
    global_args = []
    for line in logical_lines(text):
        parts = line.split(None, 1)
        ins, rest = parts[0].upper(), (parts[1] if len(parts) > 1 else "")
        if ins == "FROM":
            m = re.match(r"(?:--platform=(\S+)\s+)?(\S+)(?:\s+[Aa][Ss]\s+(\S+))?", rest)
            image = m.group(2) if m else rest
            cur = {"from": image, "platform": m.group(1) if m else None, "alias": m.group(3) if m else None,
                   "digest_pinned": "@sha256:" in image, "arg_in_from": "$" in image,
                   "copy": [], "add_remote": [], "copy_from_stage": 0, "run": 0, "run_network": 0,
                   "mounts": [], "privileged": [], "cmd": None, "entrypoint": None, "expose": [],
                   "volume": [], "healthcheck": False, "user": None, "workdir": None}
            stages.append(cur)
            continue
        if cur is None:
            if ins == "ARG":
                global_args.append(rest)
            continue
        if ins in ("COPY", "ADD"):
            if "--from=" in rest:
                cur["copy_from_stage"] += 1
            srcs = [t for t in rest.split() if not t.startswith("--")][:-1]
            if ins == "ADD" and any(s.startswith(("http://", "https://", "git@")) for s in srcs):
                cur["add_remote"].append(rest[:160])
            cur["copy"].extend(srcs[:8])
        elif ins == "RUN":
            cur["run"] += 1
            cur["mounts"] += re.findall(r"--mount=type=(\w+)", rest)
            if re.search(r"--network=host|--security=insecure|--device", rest):
                cur["privileged"].append(rest[:120])
            if NET.search(rest):
                cur["run_network"] += 1
        elif ins in ("CMD", "ENTRYPOINT"):
            cur[ins.lower()] = rest[:200]
        elif ins == "EXPOSE":
            cur["expose"] += rest.split()
        elif ins == "VOLUME":
            cur["volume"].append(rest[:120])
        elif ins == "HEALTHCHECK":
            cur["healthcheck"] = not rest.upper().startswith("NONE")
        elif ins == "USER":
            cur["user"] = rest
        elif ins == "WORKDIR":
            cur["workdir"] = rest
    final = stages[-1] if stages else None
    return {"global_args": global_args, "stages": len(stages),
            "stage_aliases": [s["alias"] for s in stages if s["alias"]],
            "bases": [s["from"] for s in stages], "all_bases_digest_pinned": bool(stages) and all(s["digest_pinned"] or s["from"] in [x["alias"] for x in stages] or s["from"] == "scratch" for s in stages),
            "arg_in_from": any(s["arg_in_from"] for s in stages),
            "remote_add": [r for s in stages for r in s["add_remote"]],
            "mounts": sorted({m for s in stages for m in s["mounts"]}),
            "privileged": [p for s in stages for p in s["privileged"]],
            "run_total": sum(s["run"] for s in stages), "run_network": sum(s["run_network"] for s in stages),
            "platforms": sorted({s["platform"] for s in stages if s["platform"]}),
            "copy_sources_sample": sorted({c for s in stages for c in s["copy"]})[:12],
            "final": final and {k: final[k] for k in ("from", "cmd", "entrypoint", "expose", "volume", "healthcheck", "user", "workdir")}}


def main():
    root_old, root_new, ids = sys.argv[1], sys.argv[2], [int(x) for x in sys.argv[3].split(",")]
    out = {}
    for i in ids:
        path = f"{root_old}/{i:02}.tar.gz" if i <= 20 else f"{root_new}/{i}.tar.gz"
        t = tarfile.open(path)
        files = []
        for m in t.getmembers():
            rel = "/".join(m.name.split("/")[1:])
            if not m.isfile() or not rel or "node_modules/" in rel or rel.count("/") > 4:
                continue
            if NAME.search(rel):
                text = t.extractfile(m).read(262144).decode("utf8", "ignore")
                files.append({"path": rel, "parsed": parse(text)})
        out[i] = {"dockerfiles": sorted(files, key=lambda f: f["path"]),
                  "root_default": any(f["path"] == "Dockerfile" for f in files)}
    print(json.dumps(out, indent=1, sort_keys=True))


if __name__ == "__main__":
    main()
