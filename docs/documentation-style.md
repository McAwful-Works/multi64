# Documentation style

How the Markdown in this repo is written, so that every page reads in one voice. The rules
that make documentation *true* live in [`CLAUDE.md`](../CLAUDE.md) and
[`CONTRIBUTING.md`](../CONTRIBUTING.md); this page is about how it reads.

## 1. Every file serves one audience first

Two very different people use this software. A player wants their cart to work. A developer
wants to know what the bytes on the wire mean. A page that tries to serve both at once serves
neither, so each file picks one and says so in its first two sentences.

| File | Serves first | Carries |
|------|--------------|---------|
| [`README.md`](../README.md) | anyone arriving | what this is, the apps, one pointer onward |
| `crates/<app>/README.md` | someone using that app | what it does, how to use it, what can go wrong; building from source **last** |
| `crates/<lib>/README.md` | a developer using that crate | what it is, its status, the spec it implements |
| [`docs/README.md`](README.md) | a developer | the map, and the order to read it in |
| [`docs/spec/`](spec/) | an implementor | normative wire behavior, nothing else |
| [`docs/integration/`](integration/) | someone putting the agent in a game | measured procedure, explicitly not normative |
| [`docs/connectors/`](connectors/) | someone running a connector | how to run one, what it expects |
| [`CONTRIBUTING.md`](../CONTRIBUTING.md) | someone changing the repo | build, layout, conventions |
| [`CLAUDE.md`](../CLAUDE.md) | an agent working here | the same, compressed, plus what is easy to get wrong |

A user-facing page may link to a spec. It may not *open* with one.

## 2. Open with what it is, not what it cannot do

The first sentence says what the thing does for the reader you picked. Caveats, hardware
status and unproven paths come after — they matter, and they are useless to someone who does
not yet know what they are reading about.

```markdown
Browse and copy the files on your cart's SD card from Windows, over the same USB cable.   ✅
Dual-pane file manager for N64 flash-cart SD contents over USB serial (FAT/exFAT via …).   ❌
```

The second is not wrong. It is a specification sentence at the top of a user's page.

**One exception, and it is [§6](#6-say-what-is-not-proven)'s.** A component that has never run
on the hardware it exists for leads with that, before saying what it does.
`crates/ed64pro-l2/README.md` opens "Experimental — never run against a cart", and should: the
reader it protects is a developer deciding whether to trust it, and the sentence that stops them
is worth more than the one that introduces it. This covers a component's own page — a crate, a
driver, a mapping — not a product's, where the caveat goes after the first sentence as above.

## 3. Titles are the product's name

`# Multi64`, `# Xfer64`, `# AP64`. No platform suffix, no parenthetical — "(Windows)" is a
requirement, so it belongs in the first line or under **Requirements**, where a reader can act
on it. Library crates take the crate name: `` # `multi64-ed64pro-l2` ``.

Headings below the title are sentence case: **Adding a game**, not **Adding A Game**.

## 4. Bold is for things the reader clicks

Bold marks a control the reader will look for on screen — **Start**, Settings → **Cart** — and
the first mention of a product name. That is all it is for. Bolding every noun in a paragraph
does not emphasize them; it removes the ability to emphasize anything.

```markdown
Press **Start**, then open the client from the Archipelago Launcher.                       ✅
**Press** the **Start** button to begin the **session** with the **cart**.                  ❌
```

Use `code` for anything the reader types or that appears in a file: commands, paths, flags,
identifiers, and wire values.

## 5. Address the reader

User-facing pages use second person and the imperative: "press Start", "plug the cart in".
Reference pages — specs, crate docs — describe behavior in the third person: "the daemon
answers", "a backend MUST NOT". Do not mix the two on one page.

## 6. Say what is not proven

Where a reader would otherwise assume something works, say that it does not, or has not been
tried. "Implemented" is not "supported"; "it opened the port" is not "it talked to a cart".
This is a truthfulness rule as much as a style one — see `CONTRIBUTING.md` — but it is also a
*placement* rule: the caveat belongs where the claim is, not in a footnote.

## 7. Commands

Shell blocks are tagged `sh` and carry no `$` prompt and no interleaved output, so any line can
be copied and run as it stands. A block holds one task: a single command, or the steps of one
task in the order they run. Separate tasks get separate blocks. When a block is a menu of
alternatives instead, such as ways to run the tests, each line says in a trailing comment what it
is for:

````markdown
```sh
cargo build -p multi64d
```

```sh
cargo test -p multi64-l3                         # one crate
cargo test -p multi64-l3 stream_decoder_resync   # one test by substring
```
````

## 8. Branding

The three apps have marks in [`branding/`](../branding/README.md). A product README opens
with its mark beside the title, and carries the same product line the app itself shows in its
window, so the page and the app agree:

```markdown
<img src="../../branding/ap64.svg" alt="" width="72" align="left" />

# AP64

**Archipelago on a real N64** · a Multi64 product

<br clear="left" />
```

Keep the mark at 72px, use an empty `alt` (it repeats the title, so a screen reader should skip
it), and clear the float before the first heading below. Library crates and `docs/` pages get no
mark: they are not products.

## 9. Links

Relative, and mind the depth — `crates/xfer64/README.md` is two levels down, so a workspace doc
is `../../docs/…`, and a single `../` silently resolves to `crates/docs/…` and 404s only once it
is on GitHub. Never wrap a link in backticks; it renders as code and stops being a link.

`sh .claude/skills/check-docs/check-docs.sh` catches both, along with British spellings and
anchors that no longer match a heading.
