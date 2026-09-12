# Public Repository Workflow

This document defines the Git and publication boundary for this Win-CodexBar fork.

## Branch model

- `main`: stable, public-ready code.
- `develop`: integration branch for work that is still under development but is safe to publish.
- `feature/*`: focused work branches created from `develop`.
- Promote changes in the order `feature/* -> develop -> main`.

Branches are for development state and feature isolation. They are not a security boundary.

## Repository and remote roles

- `origin`: the public GitHub repository owned by this project.
- `windows-upstream`: the Windows/Rust/Tauri upstream used as the code-lineage base.
- `upstream`: the original CodexBar repository for reference and upstream comparison.
- `private`: optional, only if internal-only source code must live in a separate private repository. It is not required for normal local settings or secrets.

## Public/private boundary

The public repository must never contain secrets or machine-specific private data, including in Git history.

Local-only material is ignored by Git, including:

- `.env` and `.env.*`
- `config.local.json` and `local-config.json`
- `local/`
- `secrets/`
- `*.key` and `*.pem`
- local tool/cache directories such as `.cargo-home/`, `.orchestrator/`, and `.pnpm-store/`

When a shareable configuration example is useful, commit only sanitized examples such as `config.example.json`. Real credentials and machine-specific values stay outside Git.

API keys and tokens belong in an external secret store such as SOPS, not in a Git branch.

## Project layout

Win-CodexBar keeps its existing upstream-compatible source layout instead of moving files only to match a generic template. The same publication rule applies:

- application source and tests are public,
- product documentation is public,
- local configuration and secret material are not tracked.

The existing root `README.md` and localized `README.ja-JP.md` are retained because they are part of the upstream project convention.

## Publication checklist

Before pushing a public branch:

1. Confirm only intended files are tracked/staged.
2. Scan the commit range and Git history for secrets, tokens, private keys, cookies, personal email addresses, and machine-specific paths/hosts.
3. Confirm example configuration contains dummy/sanitized values only.
4. Run `git diff --check`.
5. Run the relevant tests and production build.
6. Confirm GitHub Secret Scanning and Push Protection remain enabled where available.
7. Push feature work to `feature/*` or `develop`; promote to `main` only after review.

A private repository, when needed, is for internal-only source code. It must not be used as a substitute for proper secret storage.
