# Agent notes

- Commit messages use Conventional Commits (`feat:`, `fix:`, `docs:`, …,
  optional scope). History predating this rule was rewritten to comply; if
  you find a stray ad-hoc prefix, it is a bug, not a precedent.
- README.md is user-facing only: what it is, install, hook-up. Implementation
  detail goes in `docs/`.
- Pre-implementation design specs are archived in the superstore ledger at
  `.agents/ledger.db`, not in `docs/`.
