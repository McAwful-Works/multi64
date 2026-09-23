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

# Blank out lines inside ``` fences so example markdown in docs is not link-checked.
# Line numbers are preserved (fenced lines become empty), so reported positions stay correct.
mask_fences() {
  awk '/^[[:space:]]*```/ { infence = !infence; print ""; next } infence { print ""; next } { print }' "$1"
}

# 1. Relative markdown link targets must exist.
echo "== 1. markdown link targets =="
for f in $(md_files "$@"); do
  d=$(dirname "$f")
  mask_fences "$f" | grep -n '](' | while IFS= read -r hit; do
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

# 1b. <img src="..."> targets must exist too.
# Markdown's own image syntax is caught by check 1; these are the HTML tags a README needs when
# it wants a width or a float, which is how every product mark in this repo is placed.
echo "== 1b. image targets =="
for f in $(md_files "$@"); do
  d=$(dirname "$f")
  # Inline code spans are stripped as well as fences: a page documenting an <img> tag writes one
  # in backticks, and that is prose about markup rather than markup.
  mask_fences "$f" | sed 's/`[^`]*`//g' | grep -n -o '<img[^>]*src="[^"]*"' | while IFS= read -r hit; do
    ln=${hit%%:*}
    src=$(printf '%s
' "$hit" | sed 's/.*src="//;s/"$//')
    case "$src" in http*|data:*|'') continue ;; esac
    if [ ! -e "$d/$src" ]; then
      echo "  BROKEN  $f:$ln  ->  $src"
      echo x >> "$TMPFAIL"
    fi
  done
done

# 2. Anchors into markdown files must match a heading.
echo "== 2. link anchors =="
for f in $(md_files "$@"); do
  d=$(dirname "$f")
  mask_fences "$f" | grep -n '](' | while IFS= read -r hit; do
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
  mask_fences "$f" | grep -n '`\[[^]]*\]([^)]*)`' | while IFS= read -r hit; do
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

# 5. American English, in prose and in the names that read as prose.
#
# Searched across source as well as Markdown: a doc comment or a test name is read as often as
# a README. One pass per file with every stem in one alternation -- a pass per word would be
# ninety greps per file and take minutes. `aria-labelledby` is the one place a British-looking
# spelling is correct (it is an ARIA attribute, not a word), so it is masked first rather than
# left to trip every dialog in the repo.
echo "== 5. American English =="
LIST="$(dirname "$0")/british-spellings.txt"
if [ ! -f "$LIST" ]; then
  echo "  MISSING WORD LIST  $LIST"
  echo x >> "$TMPFAIL"
else
  ALT=$(tr -d '' < "$LIST" | grep -v '^[[:space:]]*#' | grep -v '^[[:space:]]*$'         | cut -d' ' -f1 | paste -sd'|' -)
  for f in $(git ls-files '*.md' '*.rs' '*.js' '*.mjs' '*.css' '*.html' '*.lua' '*.py' '*.c' '*.h' '*.toml' '*.sh'); do
    [ -f "$f" ] || continue
    # A file whose subject is the spelling list has to quote the spellings it rejects. Such a
    # file says so in itself, where the exemption is visible to anyone reading or reviewing it,
    # rather than being hidden in a list of paths here.
    grep -q 'check-docs: skip-spelling' "$f" && continue
    sed 's/aria-labelledby/aria-ARIAATTR/g' "$f"       | grep -n -i -o -E "$ALT" 2>/dev/null       | while IFS=: read -r ln got; do
          amer=$(tr -d '' < "$LIST" | grep -i "^$got " | head -1 | cut -d' ' -f2)
          echo "  BRITISH  $f:$ln  $got  ->  ${amer:-see $LIST}"
          echo x >> "$TMPFAIL"
        done
  done
fi

if [ -s "$TMPFAIL" ]; then
  echo
  echo "FAILED: $(wc -l < "$TMPFAIL" | tr -d ' ') problem(s) found."
  status=1
else
  echo
  echo "OK: documentation cross-references resolve and read as American English."
fi
exit $status
