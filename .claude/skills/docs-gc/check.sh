#!/usr/bin/env bash
# docs/ hygiene lint: frontmatter, index sync, root strays, stale plans, untracked docs.
# Usage: bash .claude/skills/docs-gc/check.sh   (from anywhere inside the repo)
# Exit 0 = structure clean (WARNs may still need judgment); non-zero = FAILs present.

# Root markdown allowlist — the only *.md allowed at repo root.
# Set at install time by init-memory-system; extend deliberately, never by accident.
ROOT_ALLOW="README.md CLAUDE.md AGENTS.md PLAN.md"
STALE_DAYS=45

cd "$(git rev-parse --show-toplevel)" || exit 2
fail=0

fm_field() { # fm_field <file> <key> — first frontmatter value for key
  awk -v key="$2" 'NR==1 && $0!="---"{exit} NR>1 && $0=="---"{exit} sub("^" key ":[ \t]*",""){print; exit}' "$1"
}

if [ ! -f docs/index.md ]; then
  echo "FAIL  docs/index.md missing"
  fail=1
fi

# 1. Every top-level docs/*.md (except index.md) has frontmatter with valid status + description,
#    and is listed in the index.
for f in docs/*.md; do
  [ -e "$f" ] || continue
  base=$(basename "$f")
  [ "$base" = "index.md" ] && continue
  if [ "$(head -1 "$f")" != "---" ]; then
    echo "FAIL  no frontmatter: $f"
    fail=1
    continue
  fi
  status=$(fm_field "$f" status)
  case "$status" in
    plan|active|reference) ;;
    *) echo "FAIL  bad/missing status '$status': $f"; fail=1 ;;
  esac
  desc=$(fm_field "$f" description)
  [ -z "$desc" ] && { echo "FAIL  missing description: $f"; fail=1; }
  if [ -f docs/index.md ]; then
    grep -qF "($base)" docs/index.md || { echo "FAIL  not listed in docs/index.md: $f"; fail=1; }
  fi
done

# 2. Index doesn't list ghosts.
if [ -f docs/index.md ]; then
  while read -r name; do
    [ -z "$name" ] && continue
    [ "$name" = "index.md" ] && continue
    [ -f "docs/$name" ] || { echo "FAIL  index lists missing file: $name"; fail=1; }
  done < <(grep -oE '\([A-Za-z0-9_.-]+\.md\)' docs/index.md | tr -d '()')
fi

# 3. Stale plans: status=plan with no commit touching the file for >STALE_DAYS days.
now=$(date +%s)
for f in docs/*.md; do
  [ -e "$f" ] || continue
  [ "$(basename "$f")" = "index.md" ] && continue
  [ "$(fm_field "$f" status)" = "plan" ] || continue
  last=$(git log -1 --format=%at -- "$f")
  [ -z "$last" ] && continue # untracked — reported by check 5
  age=$(( (now - last) / 86400 ))
  [ "$age" -gt "$STALE_DAYS" ] && echo "WARN  stale plan (${age}d without a commit): $f — verify shipped/dead, then delete or re-status"
done

# 4. Root strays (per the CLAUDE.md root-markdown rule).
for f in *.md; do
  [ -e "$f" ] || continue
  case " $ROOT_ALLOW " in
    *" $f "*) ;;
    *) echo "FAIL  stray root markdown: $f"; fail=1 ;;
  esac
done

# 5. Untracked files under docs/ — invisible to git history; commit (after a secret scan) or remove.
untracked=$(git ls-files --others --exclude-standard docs/ 2>/dev/null)
if [ -n "$untracked" ]; then
  echo "WARN  untracked under docs/:"
  echo "$untracked" | sed 's/^/        /'
fi

[ "$fail" -eq 0 ] && echo "OK    docs/ structure clean (any WARNs above need judgment, not scripting)"
exit $fail
