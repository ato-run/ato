#!/usr/bin/env python3
"""Collect candidate web app repos for Ato-cli validation.

This script uses GitHub CLI (gh) to search for repos matching a set of queries.
It outputs a markdown list in the format asked by the prompt.

Usage:
  python scripts/ato_cli_webapp_survey.py > survey.md
"""

import json
import subprocess
import sys

QUERIES = [
    # Node.js / JS/TS web apps
    "todo app express",
    "todo app nestjs",
    "todo app nextjs",
    "todo app nuxt",
    "kanban app nextjs",
    "kanban app react",
    "slack clone nodejs",
    "chat app nodejs",
    "dashboard app nodejs",
    "url shortener nodejs",
    "self hosted dashboard nodejs",
    "self hosted wiki nodejs",
    "self hosted chat nodejs",

    # Python
    "fastapi app",
    "django app",
    "flask app",
    "fastapi todo",
    "flask todo",
    "django blog",
    "fastapi dashboard",
    "self hosted tool python",

    # Go
    "gin web app",
    "echo web app",
    "fiber web app",
    "go dashboard",
    "go web app",
    "self hosted tool go",

    # Rust
    "axum web app",
    "actix web app",
    "rust web app",
    "rust dashboard",

    # Ruby/PHP
    "rails app",
    "laravel app",
    "swoole app",

    # Other
    "self hosted wiki",
    "self hosted kanban",
    "self hosted url shortener",
    "self hosted todo",
    "self hosted chat",
]

# We will ignore some generic repos like todomvc, etc, but let filtering happen after collection.

MAX_PER_QUERY = 30


def run_gh_search(query: str):
    cmd = [
        "gh",
        "search",
        "repos",
        query,
        "--limit",
        str(MAX_PER_QUERY),
        "--json",
        "fullName,url,description,language,stargazersCount,updatedAt,license,isArchived",
    ]
    print(f"Searching: {query}", file=sys.stderr)
    out = subprocess.check_output(cmd, text=True)
    return json.loads(out)


def normalize_repo(repo):
    # Map values to simple structure
    return {
        "fullName": repo.get("fullName"),
        "url": repo.get("url"),
        "description": (repo.get("description") or "").strip().replace("\n", " "),
        "language": repo.get("language"),
        "stars": repo.get("stargazersCount"),
        "updatedAt": repo.get("updatedAt"),
        "license": repo.get("license", {}).get("key") if repo.get("license") else None,
        "isArchived": repo.get("isArchived"),
    }


def guess_stack(repo):
    desc = (repo["description"] or "").lower()
    lang = (repo.get("language") or "").lower()

    stack = []
    if "node" in lang or "javascript" in lang or "typescript" in lang:
        stack.append("Node.js")
    if "python" in lang:
        stack.append("Python")
    if "go" in lang:
        stack.append("Go")
    if "rust" in lang:
        stack.append("Rust")
    if "ruby" in lang:
        stack.append("Ruby")
    if "php" in lang:
        stack.append("PHP")

    # quick checks for specific frameworks
    if "next" in desc or "nextjs" in desc:
        stack.append("Next.js")
    if "nuxt" in desc:
        stack.append("Nuxt")
    if "express" in desc:
        stack.append("Express")
    if "nest" in desc or "nestjs" in desc:
        stack.append("NestJS")
    if "hono" in desc:
        stack.append("Hono")
    if "fastapi" in desc:
        stack.append("FastAPI")
    if "django" in desc:
        stack.append("Django")
    if "flask" in desc:
        stack.append("Flask")
    if "uvicorn" in desc or "uv" in desc:
        stack.append("uvicorn")
    if "gin" in desc:
        stack.append("Gin")
    if "echo" in desc:
        stack.append("Echo")
    if "fiber" in desc:
        stack.append("Fiber")
    if "axum" in desc:
        stack.append("Axum")
    if "actix" in desc:
        stack.append("Actix")
    if "rails" in desc:
        stack.append("Rails")
    if "laravel" in desc:
        stack.append("Laravel")
    if "swoole" in desc:
        stack.append("Swoole")

    return stack


def guess_run_command(repo):
    # heuristics based on inferred stack
    stacks = guess_stack(repo)
    if "Next.js" in stacks:
        return "pnpm dev" if "pnpm" in repo.get("description", "") else "npm run dev"
    if "Nuxt" in stacks:
        return "pnpm dev" if "pnpm" in repo.get("description", "") else "npm run dev"
    if "Express" in stacks or "NestJS" in stacks or "Node.js" in stacks:
        # choose common scripts
        return "npm start" if "npm" in repo.get("description", "") else "node index.js"
    if "FastAPI" in stacks or "Django" in stacks or "Flask" in stacks:
        return "uvicorn main:app --reload" if "fastapi" in repo.get("description", "").lower() else "python manage.py runserver"
    if "Go" in stacks:
        return "go run ./..." if "go" in repo.get("description", "").lower() else "go run main.go"
    if "Rust" in stacks:
        return "cargo run"
    if "Rails" in stacks:
        return "bin/rails server"
    if "Laravel" in stacks:
        return "php artisan serve"
    return "(unknown, inspect repo)"


def main():
    seen = {}
    for q in QUERIES:
        try:
            results = run_gh_search(q)
        except subprocess.CalledProcessError as e:
            print(f"ERROR running search '{q}': {e}", file=sys.stderr)
            continue

        for repo in results:
            norm = normalize_repo(repo)
            key = norm["fullName"].lower()
            if key in seen:
                continue
            if norm["isArchived"]:
                continue
            # skip common library/boilerplate collections
            if "todomvc" in key:
                continue
            if "awesome" in key:
                continue
            # basic content filter: ensure it appears to be an app
            desc = norm["description"].lower()
            if any(x in desc for x in ["framework", "library", "sdk", "cli", "template", "boilerplate", "starter"]):
                # but allow if also says "app" or "self hosted"
                if not any(x in desc for x in ["app", "self hosted", "dashboard", "todo", "chat", "wiki", "kanban", "crm"]):
                    continue
            seen[key] = norm

    # Output markdown list for the first 120 entries, sorted by stars
    items = sorted(seen.values(), key=lambda r: (-(r["stars"] or 0), r["fullName"]))

    print("# Ato-cli Web App Survey (Generated)")
    print("")
    print("*Note: This list was generated by searching GitHub via gh CLI. Verify each repo for suitability.*")
    print("")

    count = 0
    for repo in items:
        if count >= 120:
            break
        count += 1
        stack = guess_stack(repo)
        if not stack:
            stack = [repo.get("language") or "Unknown"]
        run_cmd = guess_run_command(repo)
        print(f"* **[{repo['fullName']}]({repo['url']})**")
        print(f"  * 技術スタック: {', '.join(stack)}")
        print(f"  * アプリの概要: {repo['description'] or '(no description)'}")
        print(f"  * Atoでの検証価値: スター数 {repo['stars']}、最終更新 {repo['updatedAt']}、ライセンス {repo.get('license')}")
        print(f"  * 実行コマンド（推測）: {run_cmd}")
        print("")


if __name__ == "__main__":
    main()
