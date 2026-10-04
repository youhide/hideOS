# The wiki's sources

Each `.md` file here except this one is a page of the hideOS wiki,
published at <https://youhide.github.io/hideOS/wiki/>. `Home.md` is the
index.

**The wiki is updated in the same commit as the work it describes.** A
change that makes a page wrong fixes the page; a lesson learned the hard
way gets an entry in `Development-notes.md` in the commit that learned it.
The wiki summarises and links: [ARCHITECTURE.md](../../ARCHITECTURE.md),
[ROADMAP.md](../../ROADMAP.md) and the other documents stay the canonical
ones, and where they disagree, they decide.

Conventions:

- One page per file. The file name is the page's URL: `Boot-chain.md`
  becomes `Boot-chain.html`. Letters, digits, `-` and `_`.
- The page starts with `# Title`, its only first-level heading. Its first
  paragraph is its description.
- Link pages with `[[Boot chain]]`, `[[Boot chain|the boot]]`,
  `[[Boot chain#hidestage]]`, or `[text](Boot-chain.md#hidestage)`. Link the
  rest of the repository by relative path, `../../ARCHITECTURE.md`; it
  becomes a link to the file on GitHub.
- No images yet: the site loads nothing from elsewhere, and the wiki has no
  place for pictures of its own.
- Plain and precise. Say what the tree does, not what it will; mark what is
  planned as planned.

Rendering:

```sh
cargo xtask wiki            # docs/wiki/*.md → site/wiki/*.html
```

Then open `site/wiki/index.html`. `site/wiki/` is generated and not
checked in; `cargo xtask publish-site` renders it again before it
publishes, and each page's footer names the commit it was built from. A
link to a page, a section or a file that does not exist fails the build.
The generator is [crates/xtask/src/wiki.rs](../../crates/xtask/src/wiki.rs).
