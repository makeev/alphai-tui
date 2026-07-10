---
name: docs-gc
description: Periodic GC pass over docs/*.md — lint frontmatter + index sync via check.sh, verify stale plan-status docs against code/git (never against their own headers), delete fully-shipped plans (git history is the archive), catch root strays and untracked docs. Use on "почисти docs", "docs-gc", after shipping a planned feature, or roughly monthly.
---

# docs-gc — GC pass over `docs/`

## The docs/ contract (anchored in CLAUDE.md)

- Every `docs/*.md` starts with YAML frontmatter: `status: plan|active|reference` + a one-line `description`.
  - `plan` — forward work, not (fully) executed. When it fully ships → **delete the file** (git history is the archive) or, if it turned into a living runbook/reference, re-status it with an honest status paragraph up top.
  - `active` — living doc, continuously updated (playbooks, workstreams).
  - `reference` — stable record: design records, runbooks, analyses with a decision taken.
- `docs/index.md` — one line per file, grouped by status. Add/remove a doc → update the index.
- Root `*.md` does not grow (allowlist lives in CLAUDE.md and `check.sh`).

## Procedure

1. Run `bash .claude/skills/docs-gc/check.sh` — mechanical lint: frontmatter present + valid status/description, index sync both ways, root strays, stale plans (>45 days without a commit), untracked files under `docs/`. Fix FAIL items immediately.
2. For every stale-plan WARN (and, on a full pass, every `status: plan` file): verify against **reality, not the file's own header** — grep the code for the feature, check `git log`, memory, prod if applicable. Three outcomes:
   - fully shipped → `git rm`; unfinished tails (if any) → the TODO queue in the session memory dir; fix any references to the file in code/docs in the same commit;
   - shipped but the file became a runbook → `status: reference` + honest status note up top;
   - not shipped → keep; refresh the in-body status line if it went stale.
3. **Before deleting any doc**: `grep -rn '<FILENAME>' .` (excluding node_modules/.git). Code comments, other docs, and issues/ may reference `docs/` files by name. References found → either keep as `reference` or rewrite the referents in the same commit.
4. Untracked files under `docs/` → commit them (after a secret scan: `grep -inE 'password|api[_-]?key|token|secret'`) or remove them.
5. Update `docs/index.md`; if the rules themselves changed, update CLAUDE.md. One commit per GC pass, directly to the default branch, no attribution trailers.

## Where content goes when a file is deleted

- unfinished plan tails → the TODO queue in the session memory dir;
- incident material → `issues/YYYY-MM-DD-slug.md`;
- durable facts worth recalling → memory (project/reference);
- the plan text itself → nowhere: git history is the archive.
