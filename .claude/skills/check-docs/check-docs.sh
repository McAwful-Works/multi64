#!/usr/bin/env sh
# Validate documentation cross-references in the multi64 repo.
# Usage: sh .claude/skills/check-docs/check-docs.sh [path ...]
# Exits 1 if any problem is found. Reports file:line for every hit.
TMPFAIL=$(mktemp); trap "rm -f $TMPFAIL" EXIT

set -u
cd "$(git rev-parse --show-toplevel)" || exit 2
status=0

md_files() { if [ "$#" -gt 0 ]; then for p in "$@"; do echo "$p"; done; else git ls-files '*.md'; fi; }

slugify() { # GitHub-style heading anchor
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' \
    | sed -e 's/^#*[[:space:]]*//' -e 's/[^a-z0-9 -]//g' -e 's/[[:space:]]\{1,\}/-/g'
}

# 1. Relative markdown link targets must exist.
echo "== 1. markdown link targets =="
for f in $(md_files "$@"); do
  d=$(dirname "$f")
  grep -n '](' "$f" | while IFS= read -r hit; do
    ln=${hit%%:*}
    printf '%s\n' "$hit" | grep -o '](\([^)]*\))' | sed 's/^](//;s/)$//' | while IFS= read -r link; do
      case "$link" in http*|mailto:*|'#'*|'') continue ;; esac
      target=${link%%#*}
      [ -z "$target" ] && continue
      if [ ! -e "$d/$target" ]; then
        echo "  BROKEN  $f:$ln  ->  $link"
        echo x >> "$TMPFAIL"
      fi
    done
  done
done

# 2. Anchors into markdown files must match a heading.
echo "== 2. link anchors =="
for f in $(md_files "$@"); do
  d=$(dirname "$f")
  grep -n '](' "$f" | while IFS= read -r hit; do
    ln=${hit%%:*}
    printf '%s\n' "$hit" | grep -o '](\([^)]*\))' | sed 's/^](//;s/)$//' | while IFS= read -r link; do
      case "$link" in http*|mailto:*) continue ;; esac
      case "$link" in *'#'*) ;; *) continue ;; esac
      anchor=${link#*#}
      target=${link%%#*}
      [ -z "$anchor" ] && continue
      if [ -z "$target" ]; then page="$f"; else page="$d/$target"; fi
      case "$page" in *.md) ;; *) continue ;; esac
      [ -e "$page" ] || continue
      found=0
      while IFS= read -r heading; do
        [ "$(slugify "$heading")" = "$anchor" ] && { found=1; break; }
      done <<EOF
$(grep '^#' "$page")
EOF
      if [ "$found" -eq 0 ]; then
        echo "  NO ANCHOR  $f:$ln  ->  $link"
        echo x >> "$TMPFAIL"
      fi
    done
  done
done

# 3. Links wrapped in backticks render as literal text, not links.
echo "== 3. backtick-wrapped links =="
for f in $(md_files "$@"); do
  grep -n '`\[[^]]*\]([^)]*)`' "$f" | while IFS= read -r hit; do
    echo "  NOT A LINK  $f:${hit%%:*}  (remove the surrounding backticks)"
    echo x >> "$TMPFAIL"
  done
done

# 4. docs/spec paths cited from source must exist.
echo "== 4. spec paths cited in source =="
grep -rn -o 'docs/spec/[A-Za-z0-9._-]*\.md' --include='*.rs' --include='*.js' . 2>/dev/null \
  | while IFS= read -r hit; do
      path=${hit##*:}
      loc=${hit%:*}
      [ -e "$path" ] || { echo "  MISSING  $loc  ->  $path"; echo x >> "$TMPFAIL"; }
    done

if [ -s "$TMPFAIL" ]; then
  echo
  echo "FAILED: $(wc -l < "$TMPFAIL" | tr -d ' ') problem(s) found."
  status=1
else
  echo
  echo "OK: all documentation cross-references resolve."
fi
exit $status
