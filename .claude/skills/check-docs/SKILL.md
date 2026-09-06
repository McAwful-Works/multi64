---
name: check-docs
description: Validate documentation cross-references across the repo - relative markdown links, heading anchors, backtick-wrapped links that silently fail to render, and docs/spec/ paths cited from Rust or JS. Use before merging any change that touches .md files, renames a spec, or moves a crate, and whenever asked to check or fix documentation links.
---

# Check documentation cross-references

This repo carries heavy interlinked docs: `docs/spec/` is normative and is cited from README files, from Rust `//!` comments, and from other specs. Renaming a spec or moving a file breaks references that nothing in CI checks.

## Run it

```sh
sh .claude/skills/check-docs/check-docs.sh
```

Optionally pass specific markdown files to limit checks 1-3; check 4 always sweeps the whole repo. Exit code is 1 when anything fails, so it composes into a shell chain.

## What it catches

1. **Relative link targets that do not exist.** Most often a wrong `../` depth. `crates/xfer64/README.md` is two levels down, so workspace docs are `../../docs/spec/...`; a single `../` silently resolves to `crates/docs/spec/...` and 404s on GitHub.
2. **Anchors with no matching heading**, using GitHub's slug rules (lowercase, punctuation stripped, spaces to hyphens). Catches `docs/README.md#flash-carts-l2-backends` drifting when a heading is reworded.
3. **Links wrapped in backticks**, which render as literal code rather than a link:

   ```
   `[text](url)`
   ```

   Easy to introduce when converting a table cell to code style, and invisible unless you look at the rendered page. Examples inside fenced code blocks are masked before checking, so a doc may show the pattern without tripping the check.
4. **`docs/spec/*.md` paths cited from `.rs` or `.js`** that no longer exist, e.g. a module doc pointing at a spec that was renamed.

## Fixing what it reports

Confirm the intended target before editing — a broken link is sometimes a renamed file (fix the name) and sometimes a wrong depth (fix the `../`), and the two look alike. `git log --diff-filter=R --name-status -- docs/spec/` shows past spec renames.

When a spec genuinely no longer exists, do not just delete the link: `docs/README.md` is the spec map and `docs/spec/README.md` is the index, so both usually need the same edit.
