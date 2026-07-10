# tr-monitor

Terminal TUI for watching ticker prices (Rust, ratatui). Architecture and
extension points: `README.md`. Roadmap: `PLAN.md`.

## Knowledge system (docs / issues / plan)

How durable knowledge is organized in this repo — four layers, each with its own contract:

- **`CLAUDE.md` / `AGENTS.md`** — instructions and invariants for agents; read every session. A code/architecture invariant discovered while working goes here.
- **`docs/`** — living references and forward plans. Every `docs/*.md` starts with YAML frontmatter — `status: plan|active|reference` + a one-line `description` — and has a line in `docs/index.md`; update both when adding or removing a doc.
  - `plan` — forward work, not (fully) executed. When it fully ships → **delete the file** (git history is the archive), or re-status to `reference` if it turned into a living runbook. Before deleting any doc, grep the repo for its filename — code comments and other docs may reference it by name.
  - `active` — living doc, continuously updated (playbooks, workstreams).
  - `reference` — stable record: design records, runbooks, analyses with a decision taken.
  - Periodic cleanup = the **`docs-gc` skill** (`.claude/skills/docs-gc/`; `check.sh` is the mechanical lint: frontmatter, index sync both ways, root strays, stale plans).
- **`issues/`** — dated post-mortems, one file per notable bug/incident/investigation: `issues/YYYY-MM-DD-slug.md`, structured **symptom → root cause → fix → how it was verified**. This is the durable history — write it so a future human or agent can re-check the reasoning (not just the diff) and avoid re-debugging the same thing. Add the file in the same commit as the fix.
- **`PLAN.md`** (root) — the phased roadmap with status marks. Details of a non-trivial phase go to a `docs/` plan file linked from it.

**Root markdown does not grow.** Root holds only: `README.md`, `CLAUDE.md`, `AGENTS.md`, `PLAN.md`. Everything else has a home: dated investigations/post-mortems → `issues/`; living references and plans → `docs/` (frontmatter + index line); session-temporary output → the scratchpad, never the repo. A plan that shipped or a review whose findings are all fixed gets **deleted**, not archived.

**Where to write something new:** code/architecture invariant → `CLAUDE.md`/`AGENTS.md`; incident write-up → `issues/`; plan or reference → `docs/` (frontmatter + index line); roadmap-level change → `PLAN.md`; "don't forget to do/check X" → the session memory TODO queue (outside the repo), not a repo file.
