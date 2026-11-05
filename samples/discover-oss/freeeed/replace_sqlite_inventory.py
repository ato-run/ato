#!/usr/bin/env python3
"""Record pinned dependency replacements in a disposable inventory."""

import argparse
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("inventory", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--guava", action="store_true", help="also replace Guava and add failureaccess")
    parser.add_argument("--commons-io", action="store_true", help="also replace Commons IO")
    args = parser.parse_args()
    data = json.loads(args.inventory.read_text())
    if data.get("kind") != "preliminary-maven-runtime-inventory/1":
        raise ValueError("unexpected inventory")
    matches = [item for item in data["dependencies"] if item["coordinate"] == "org.xerial:sqlite-jdbc:3.41.2.1"]
    if len(matches) != 1 or matches[0]["sha256"] != "b3141057bfb685ea9f9ab200c2158bc28b47c4a2d42c88fcb0a1a00954fad26d":
        raise ValueError("unexpected original sqlite-jdbc")
    data["dependencies"].remove(matches[0])
    replacement = dict(matches[0])
    replacement.update(coordinate="org.xerial:sqlite-jdbc:3.41.2.2",
                       sha256="0cdab410947e04b6743df99cf1543267ddd107357d6f76948d145be590fd497d",
                       license_pom="org.xerial:sqlite-jdbc:3.41.2.2")
    data["dependencies"].append(replacement)
    if args.guava:
        matches = [item for item in data["dependencies"] if item["coordinate"] == "com.google.guava:guava:19.0"]
        if len(matches) != 1 or matches[0]["sha256"] != "58d4cc2e05ebb012bbac568b032f75623be1cb6fb096f3c60c72a86f7f057de4":
            raise ValueError("unexpected original Guava")
        data["dependencies"].remove(matches[0])
        guava = dict(matches[0])
        guava.update(coordinate="com.google.guava:guava:33.8.0-jre",
                     sha256="97b79ec3f2c0a6313a0c2df51ce1abdf8907e6531803467b81efcaaf99b35161",
                     license_pom="com.google.guava:guava-parent:33.8.0-jre")
        data["dependencies"].append(guava)
        failureaccess = dict(guava)
        failureaccess.update(coordinate="com.google.guava:failureaccess:1.0.3",
                             sha256="cbfc3906b19b8f55dd7cfd6dfe0aa4532e834250d7f080bd8d211a3e246b59cb",
                             license_pom="com.google.guava:guava-parent:33.4.0-android")
        data["dependencies"].append(failureaccess)
    if args.commons_io:
        matches = [item for item in data["dependencies"] if item["coordinate"] == "commons-io:commons-io:2.5"]
        if len(matches) != 1 or matches[0]["sha256"] != "a10418348d234968600ccb1d988efcbbd08716e1d96936ccc1880e7d22513474":
            raise ValueError("unexpected original Commons IO")
        data["dependencies"].remove(matches[0])
        commons_io = dict(matches[0])
        commons_io.update(coordinate="commons-io:commons-io:2.14.0",
                          sha256="e35d1df7232568ed4b81d9a9ed94a0159b3ff6cf1915cc21eef5cb55d7b157a2",
                          license_pom="org.apache:apache:30")
        data["dependencies"].append(commons_io)
    data["dependencies"].sort(key=lambda item: item["coordinate"])
    data["warning"] = ("Experimental text-only variant replaces sqlite-jdbc 3.41.2.1 with hash-pinned 3.41.2.2"
                       + (" and Guava 19.0 with 33.8.0-jre plus failureaccess 1.0.3" if args.guava else "")
                       + (" and Commons IO 2.5 with 2.14.0" if args.commons_io else "")
                       + "; this is not the upstream dependency tree.")
    tmp = (Path.cwd() / ".tmp").resolve(strict=True)
    if not args.output.resolve().is_relative_to(tmp):
        raise ValueError("output must be under .tmp")
    args.output.write_text(json.dumps(data, indent=2) + "\n")


if __name__ == "__main__":
    main()
