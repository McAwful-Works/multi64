---
name: check-docs
description: Check the repo's documentation mechanically - relative markdown links, heading anchors, backtick-wrapped links that silently fail to render, docs/spec/ paths cited from Rust or JS, and American English across Markdown and source. Use before merging any change that touches .md files or writes prose anywhere, renames a spec, or moves a crate, and whenever asked to check or fix documentation links or spelling.
---

# Check documentation

<!-- check-docs: skip-spelling -- this page quotes the spellings check 5 rejects. Any file
     may exempt itself this way; doing so is visible in the diff, which is the point. -->

This repo carries heavy interlinked docs: `docs/spec/` is normative and is cited from README files, from Rust `//!` comments, and from other specs. Renaming a spec or moving a file breaks references that nothing in CI checks.

## Run it

```sh
sh .claude/skills/check-docs/check-docs.sh
```

Optionally pass specific markdown files to limit checks 1-3; checks 4 and 5 always sweep the whole repo. Exit code is 1 when anything fails, so it composes into a shell chain.

## What it catches

1. **Relative link targets that do not exist.** Most often a wrong `../` depth. `crates/xfer64/README.md` is two levels down, so workspace docs are `../../docs/spec/...`; a single `../` silently resolves to `crates/docs/spec/...` and 404s on GitHub.
2. **Anchors with no matching heading**, using GitHub's slug rules (lowercase, punctuation stripped, spaces to hyphens). Catches `docs/README.md#flash-carts-l2-backends` drifting when a heading is reworded.
3. **Links wrapped in backticks**, which render as literal code rather than a link:

   ```
   `[text](url)`
   ```

   Easy to introduce when converting a table cell to code style, and invisible unless you look at the rendered page. Examples inside fenced code blocks are masked before checking, so a doc may show the pattern without tripping the check.
4. **`docs/spec/*.md` paths cited from `.rs` or `.js`** that no longer exist, e.g. a module doc pointing at a spec that was renamed.
5. **British spellings**, in Markdown and in source. Comments, doc comments, test names and
   error messages get read as often as a README, so they are searched too. The pairs live in
   [`british-spellings.txt`](british-spellings.txt), matched case-insensitively as substrings,
   so one stem covers a whole family (`recognis` catches recognise, recognised, unrecognised).

   That only works for stems no American word contains, which is why the list is fussier than
   it looks: `analys` would hit *analysis*, `realis` would hit *realistic*, `programme` would
   hit *programmer*. Those are listed as explicit forms instead. `grey` is absent on purpose —
   it is a real CSS color keyword, sitting beside `gray` in Multi64's palette checker — and so
   is `cancelled`, which is an accepted American variant and a Rust identifier here.
   `aria-labelledby` is masked before the search: it is an ARIA attribute, not a word.

   Add to the list rather than loosening the matching. A false positive here is worse than a
   miss, because it teaches everyone to ignore the check.

## Fixing what it reports

Confirm the intended target before editing — a broken link is sometimes a renamed file (fix the name) and sometimes a wrong depth (fix the `../`), and the two look alike. `git log --diff-filter=R --name-status -- docs/spec/` shows past spec renames.

When a spec genuinely no longer exists, do not just delete the link: `docs/README.md` is the spec map and `docs/spec/README.md` is the index, so both usually need the same edit.
