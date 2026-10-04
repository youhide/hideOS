//! `cargo xtask wiki`: the project wiki, rendered.
//!
//! The pages are Markdown in `docs/wiki/`, one file per page, `Home.md`
//! first; they are written in the same commit as the work they describe.
//! This renders them into `site/wiki/`, in the site's own look
//! (`site/style.css`), with every page listed beside every page, anchors
//! on the headings, and a footer naming the commit the page was built
//! from. `publish-site` runs it, so what is published is what is
//! committed.
//!
//! `site/wiki/` is not checked in. A page names the commit it was built
//! from, and a page checked in would always name the commit before the one
//! that holds it.
//!
//! Links between pages are `[[Page name]]`, `[[Page name|text]]` or
//! `[text](Page-name.md#section)`; links to the rest of the repository are
//! relative paths, `../../ARCHITECTURE.md`, which become links to GitHub.
//! A link to a page, a section or a file that does not exist fails the
//! build, naming the page it is on. The output depends only on the pages
//! and the commit: files are read in name order, and nothing records when.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use pulldown_cmark::{CowStr, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd};

/// Where the pages are, and where they go, relative to the workspace.
const SOURCE: &str = "docs/wiki";
const OUTPUT: &str = "site/wiki";
/// What a link to the rest of the repository points at.
const REPOSITORY: &str = "https://github.com/youhide/hideOS";
/// Where the site is published, for canonical URLs.
const SITE: &str = "https://youhide.github.io/hideOS";
/// The page every other is reached from.
const HOME: &str = "Home";

/// `cargo xtask wiki`.
pub fn run() -> Result<(), String> {
    let root = super::workspace_root()?;
    let pages = generate(&root)?;
    println!("wiki: {pages} pages in {OUTPUT}/");
    Ok(())
}

/// Renders `docs/wiki/*.md` into `site/wiki/`, replacing what was there.
/// Returns the number of pages.
pub fn generate(root: &Path) -> Result<usize, String> {
    let sources = read_sources(&root.join(SOURCE))?;
    if !sources.contains_key(HOME) {
        return Err(format!("{SOURCE}/{HOME}.md is missing: it is the index"));
    }
    let names: BTreeSet<String> = sources.keys().cloned().collect();
    let mut pages = BTreeMap::new();
    for (stem, text) in &sources {
        let page =
            render(stem, text, &names, root).map_err(|e| format!("{SOURCE}/{stem}.md: {e}"))?;
        pages.insert(stem.clone(), page);
    }
    check_links(&pages)?;
    let order = sidebar_order(&pages);
    let built = built_from(root)?;

    let out = root.join(OUTPUT);
    if out.exists() {
        fs::remove_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    for page in pages.values() {
        let html = page_html(page, &order, &pages, &built);
        let file = out.join(href(&page.stem));
        fs::write(&file, html).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    Ok(pages.len())
}

/// Every page's Markdown, by file stem. `README.md` is about the directory,
/// for someone browsing it on GitHub, and is not a page.
fn read_sources(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut sources = BTreeMap::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem == "README" {
            continue;
        }
        if !stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "{}: page names are letters, digits, `-` and `_`, which become its URL",
                path.display()
            ));
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        sources.insert(stem.to_owned(), text);
    }
    Ok(sources)
}

/// One page, rendered, with what the rest of the wiki needs to know of it.
struct Page {
    stem: String,
    title: String,
    /// The first paragraph, as plain text, for the description.
    summary: String,
    body: String,
    /// The second-level headings, as (text, id): the page's own contents.
    sections: Vec<(String, String)>,
    /// Every id on the page, for links to its sections.
    ids: BTreeSet<String>,
    /// Links to wiki pages, in the order they appear: (page, fragment).
    links: Vec<(String, Option<String>)>,
}

fn render(stem: &str, text: &str, names: &BTreeSet<String>, root: &Path) -> Result<Page, String> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_WIKILINKS
        | Options::ENABLE_SMART_PUNCTUATION;
    let mut events: Vec<Event> = Vec::new();
    let mut title = None;
    let mut summary = None;
    let mut sections = Vec::new();
    let mut ids = BTreeSet::new();
    let mut links = Vec::new();
    // A heading's events are held until its end, when its text, and so its
    // id, is known.
    let mut heading: Option<(HeadingLevel, Vec<Event>)> = None;
    // The first paragraph's text, gathered as it goes past.
    let mut paragraph: Option<String> = None;
    // `[[Page#section]]` with no text of its own: the parser gives it
    // `Page#section` as its text, and it reads as `Page`.
    let mut wiki_text: Option<String> = None;

    for event in Parser::new_ext(text, options) {
        let event = match event {
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title: link_title,
                id,
            }) => {
                let wiki = matches!(link_type, LinkType::WikiLink { .. });
                if matches!(link_type, LinkType::WikiLink { has_pothole: false })
                    && let Some((page, section)) = dest_url.split_once('#')
                {
                    let page = page.trim();
                    wiki_text = Some(if page.is_empty() { section } else { page }.to_owned());
                }
                let dest = resolve(&dest_url, wiki, stem, names, root, &mut links)?;
                Event::Start(Tag::Link {
                    link_type,
                    dest_url: CowStr::from(dest),
                    title: link_title,
                    id,
                })
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                return Err(format!(
                    "an image ({dest_url}): the wiki has none yet, and the site loads nothing from elsewhere"
                ));
            }
            Event::Text(_) if wiki_text.is_some() => {
                Event::Text(CowStr::from(wiki_text.take().unwrap_or_default()))
            }
            other => other,
        };

        if let Some((level, held)) = &mut heading {
            if let Event::End(TagEnd::Heading(_)) = event {
                let level = *level;
                let held = std::mem::take(held);
                heading = None;
                let text = plain_text(&held);
                if level == HeadingLevel::H1 {
                    if title.is_some() {
                        return Err(format!("a second title, `# {text}`: a page has one"));
                    }
                    title = Some(text.clone());
                }
                let id = unique_id(&slug(&text), &mut ids);
                if level == HeadingLevel::H2 {
                    sections.push((text, id.clone()));
                }
                events.push(Event::Start(Tag::Heading {
                    level,
                    id: Some(CowStr::from(id.clone())),
                    classes: Vec::new(),
                    attrs: Vec::new(),
                }));
                // The Markdown's own hashes, as the link to the section.
                if level != HeadingLevel::H1 {
                    let hashes = "#".repeat(level as usize);
                    events.push(Event::InlineHtml(CowStr::from(format!(
                        "<a class=\"h\" href=\"#{id}\" aria-hidden=\"true\" tabindex=\"-1\">{hashes}</a> "
                    ))));
                }
                events.extend(held);
                events.push(Event::End(TagEnd::Heading(level)));
            } else {
                held.push(event);
            }
            continue;
        }

        match &event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some((*level, Vec::new()));
                continue;
            }
            Event::Start(Tag::Paragraph) if summary.is_none() => paragraph = Some(String::new()),
            Event::End(TagEnd::Paragraph) => {
                if let Some(text) = paragraph.take() {
                    summary = Some(text);
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some(text) = &mut paragraph {
                    text.push_str(t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(text) = &mut paragraph {
                    text.push(' ');
                }
            }
            _ => {}
        }

        // A table scrolls in its own box on a narrow screen, rather than
        // making the page scroll sideways.
        match event {
            Event::Start(Tag::Table(_)) => {
                events.push(Event::Html(CowStr::from("<div class=\"table-wrap\">\n")));
                events.push(event);
            }
            Event::End(TagEnd::Table) => {
                events.push(event);
                events.push(Event::Html(CowStr::from("</div>\n")));
            }
            other => events.push(other),
        }
    }

    let title = title.ok_or("no title: a page starts with `# Title`")?;
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, events.into_iter());
    Ok(Page {
        stem: stem.to_owned(),
        title,
        summary: summary.map(|s| shorten(&s, 160)).unwrap_or_default(),
        body,
        sections,
        ids,
        links,
    })
}

/// Where a link goes, as the rendered page links it. Records links to wiki
/// pages, to be checked once every page's ids are known.
fn resolve(
    dest: &str,
    wiki: bool,
    from: &str,
    names: &BTreeSet<String>,
    root: &Path,
    links: &mut Vec<(String, Option<String>)>,
) -> Result<String, String> {
    let (path, fragment) = match dest.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment.to_owned())),
        None => (dest, None),
    };
    let with_fragment = |base: String| match &fragment {
        Some(f) => format!("{base}#{f}"),
        None => base,
    };
    let mut page = |stem: &str| -> Result<String, String> {
        if !names.contains(stem) {
            return Err(format!(
                "a link to `{dest}`: there is no page {SOURCE}/{stem}.md"
            ));
        }
        links.push((stem.to_owned(), fragment.clone()));
        Ok(with_fragment(href(stem)))
    };

    if wiki {
        let name = path.trim();
        if name.is_empty() {
            return page(from).map(|_| with_fragment(String::new()));
        }
        return page(&name.replace(' ', "-"));
    }
    if dest.contains("://") || dest.starts_with("mailto:") {
        return Ok(dest.to_owned());
    }
    if path.is_empty() {
        // `#section`, on this page.
        return page(from).map(|_| with_fragment(String::new()));
    }
    if path.starts_with('/') {
        return Err(format!(
            "a link to `{dest}`: links are relative, to a page or to a file in the repository"
        ));
    }
    if !path.contains('/')
        && let Some(stem) = path.strip_suffix(".md")
        && stem != "README"
    {
        return page(stem);
    }
    // A file elsewhere in the repository, relative to this directory.
    let mut parts: Vec<String> = SOURCE.split('/').map(str::to_owned).collect();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(format!("a link to `{dest}`, outside the repository"));
                }
            }
            Component::CurDir => {}
            _ => return Err(format!("a link to `{dest}`: not a relative path")),
        }
    }
    let relative = parts.join("/");
    let target: PathBuf = root.join(&relative);
    let kind = if target.is_dir() {
        "tree"
    } else if target.is_file() {
        "blob"
    } else {
        return Err(format!("a link to `{dest}`: there is no {relative}"));
    };
    Ok(with_fragment(format!(
        "{REPOSITORY}/{kind}/main/{relative}"
    )))
}

/// Every link to a page goes to a page that is there, and every link to a
/// section to a section that is.
fn check_links(pages: &BTreeMap<String, Page>) -> Result<(), String> {
    let mut broken = Vec::new();
    for page in pages.values() {
        for (target, fragment) in &page.links {
            let Some(fragment) = fragment else { continue };
            let found = pages.get(target).is_some_and(|t| t.ids.contains(fragment));
            if !found {
                broken.push(format!(
                    "{SOURCE}/{}.md: a link to {target}.md#{fragment}, which has no such section",
                    page.stem
                ));
            }
        }
    }
    if broken.is_empty() {
        Ok(())
    } else {
        Err(broken.join("\n"))
    }
}

/// The order pages are listed in: Home, then the pages in the order Home
/// links them, then any it does not link, by name. The index decides, so
/// no list has to be kept beside it.
fn sidebar_order(pages: &BTreeMap<String, Page>) -> Vec<String> {
    let mut order = vec![HOME.to_owned()];
    if let Some(home) = pages.get(HOME) {
        for (target, _) in &home.links {
            if !order.contains(target) {
                order.push(target.clone());
            }
        }
    }
    for stem in pages.keys() {
        if !order.contains(stem) {
            order.push(stem.clone());
        }
    }
    order
}

/// What the footer says the pages were built from: the commit, and whether
/// the pages had changes not yet in it.
fn built_from(root: &Path) -> Result<String, String> {
    let git = |args: &[&str]| -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .map_err(|e| format!("running git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    let commit = git(&["rev-parse", "--short", "HEAD"])?;
    let dirty = !git(&["status", "--porcelain", "--", SOURCE])?.is_empty();
    Ok(if dirty {
        format!("<code>{commit}</code>, with changes to {SOURCE} not yet committed")
    } else {
        format!("<code>{commit}</code>")
    })
}

/// A page's file name in the site. Home is the directory's index, so the
/// wiki's address is `…/wiki/`.
fn href(stem: &str) -> String {
    if stem == HOME {
        "index.html".to_owned()
    } else {
        format!("{stem}.html")
    }
}

/// A heading's id, the way GitHub makes one, so that a link written
/// against the Markdown on GitHub works here too: lower case, spaces to
/// hyphens, punctuation dropped.
fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
        } else if c == ' ' || c == '-' {
            slug.push('-');
        } else if c == '_' {
            slug.push('_');
        }
    }
    if slug.is_empty() {
        "section".to_owned()
    } else {
        slug
    }
}

/// `id`, or `id-1`, `id-2`… when the page already has it.
fn unique_id(id: &str, taken: &mut BTreeSet<String>) -> String {
    let mut candidate = id.to_owned();
    let mut n = 0;
    while taken.contains(&candidate) {
        n += 1;
        candidate = format!("{id}-{n}");
    }
    taken.insert(candidate.clone());
    candidate
}

/// The text of a run of inline events, without markup.
fn plain_text(events: &[Event]) -> String {
    let mut text = String::new();
    for event in events {
        match event {
            Event::Text(t) | Event::Code(t) => text.push_str(t),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            _ => {}
        }
    }
    text.trim().to_owned()
}

/// `text` with its whitespace collapsed, cut at a word to at most `max`
/// characters.
fn shorten(text: &str, max: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out = String::new();
    for word in words {
        let len = out.chars().count() + word.chars().count() + 1;
        if len > max {
            out.push('…');
            return out;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

/// The list of pages, with this page's sections under it.
fn toc_html(current: &Page, order: &[String], pages: &BTreeMap<String, Page>) -> String {
    let mut html = String::from(
        "<nav class=\"toc\" aria-label=\"Wiki pages\">\n<p class=\"toc__title\"><span class=\"sig\">$</span>ls wiki/</p>\n<ol class=\"toc__pages\">\n",
    );
    for stem in order {
        let Some(page) = pages.get(stem) else {
            continue;
        };
        let here = page.stem == current.stem;
        html.push_str(&format!(
            "<li><a href=\"{}\"{}>{}</a>",
            href(&page.stem),
            if here { " aria-current=\"page\"" } else { "" },
            escape(&page.title)
        ));
        if here && !page.sections.is_empty() {
            html.push_str("\n<ol class=\"toc__sections\">\n");
            for (text, id) in &page.sections {
                html.push_str(&format!(
                    "<li><a href=\"#{id}\">{}</a></li>\n",
                    escape(text)
                ));
            }
            html.push_str("</ol>\n");
        }
        html.push_str("</li>\n");
    }
    html.push_str("</ol>\n</nav>");
    html
}

/// A whole page: the site's header, the pages, the page, and a footer that
/// says what it was built from. The header's links are the front page's
/// (site/index.html).
fn page_html(page: &Page, order: &[String], pages: &BTreeMap<String, Page>, built: &str) -> String {
    let toc = toc_html(page, order, pages);
    let title = escape(&page.title);
    let canonical = if page.stem == HOME {
        format!("{SITE}/wiki/")
    } else {
        format!("{SITE}/wiki/{}", href(&page.stem))
    };
    let links = "\
        <a href=\"index.html\" aria-current=\"page\">./wiki</a>\n\
        <a href=\"https://github.com/youhide/hideOS/blob/main/ARCHITECTURE.md\">./architecture</a>\n\
        <a href=\"https://github.com/youhide/hideOS/blob/main/ROADMAP.md\">./roadmap</a>\n\
        <a href=\"https://github.com/youhide/hideOS\">./github</a>";
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · hideOS wiki</title>
<meta name="description" content="{summary}">
<meta name="author" content="Youri Mattar">
<meta name="theme-color" content="#282a36">
<link rel="canonical" href="{canonical}">
<link rel="icon" href="../favicon.svg" type="image/svg+xml">
<link rel="icon" href="../favicon-32x32.png" sizes="32x32" type="image/png">
<link rel="apple-touch-icon" href="../apple-touch-icon.png">
<link rel="stylesheet" href="../style.css">
</head>
<body>
<a class="skip" href="#main">Skip to content</a>

<header class="nav">
  <div class="wrap wrap--wide nav__inner">
    <a class="nav__brand" href="../">~/<span>hideOS</span></a>
    <nav class="nav__links" aria-label="Project">
{links}
    </nav>
    <details class="nav__menu">
      <summary>menu</summary>
      <nav aria-label="Project">
{links}
      </nav>
    </details>
  </div>
</header>

<div class="wrap wrap--wide wiki">
  <details class="wiki__toc wiki__toc--compact">
    <summary>wiki/ <b>{title}</b></summary>
{toc}
  </details>
  <aside class="wiki__toc wiki__toc--side">
{toc}
  </aside>
  <main id="main" class="wiki__page">
    <p class="prompt muted"><span class="prompt__sign">$</span>cat wiki/{stem}.md</p>
    <article class="prose">
{body}    </article>
  </main>
</div>

<footer class="footer">
  <div class="wrap wrap--wide">
    <div class="footer__card">
      <div class="footer__accent"></div>
      <div class="footer__body">
        <p># built from commit {built}. source: <a href="{REPOSITORY}/blob/main/{SOURCE}/{stem}.md">{SOURCE}/{stem}.md</a>.</p>
        <p># the wiki changes in the same commit as the work it describes; where it and <a href="{REPOSITORY}/blob/main/ARCHITECTURE.md">ARCHITECTURE.md</a> disagree, ARCHITECTURE.md decides.</p>
        <p>hideOS is open source under MIT or Apache-2.0, by <a href="https://youhide.com.br">youhide</a>.</p>
      </div>
    </div>
  </div>
</footer>
</body>
</html>
"##,
        summary = escape(&page.summary),
        stem = page.stem,
        body = page.body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_githubs() {
        assert_eq!(
            slug("Boot counting, in file names"),
            "boot-counting-in-file-names"
        );
        assert_eq!(slug("`hide update`"), "hide-update");
        assert_eq!(slug("LUKS2 — and the TPM"), "luks2--and-the-tpm");
    }

    #[test]
    fn repeated_headings_get_numbered() {
        let mut taken = BTreeSet::new();
        assert_eq!(unique_id("notes", &mut taken), "notes");
        assert_eq!(unique_id("notes", &mut taken), "notes-1");
        assert_eq!(unique_id("notes", &mut taken), "notes-2");
    }

    #[test]
    fn links_become_pages_and_github() {
        let root = super::super::workspace_root().unwrap();
        let names: BTreeSet<String> = ["Home", "Boot-chain"].map(String::from).into();
        let mut links = Vec::new();
        let mut go =
            |dest: &str, wiki: bool| resolve(dest, wiki, "Home", &names, &root, &mut links);
        assert_eq!(go("Boot chain", true).unwrap(), "Boot-chain.html");
        assert_eq!(
            go("Boot-chain.md#hidestage", false).unwrap(),
            "Boot-chain.html#hidestage"
        );
        assert_eq!(go("Home.md", false).unwrap(), "index.html");
        assert_eq!(go("#status", false).unwrap(), "#status");
        assert_eq!(
            go("../../ARCHITECTURE.md#updates", false).unwrap(),
            "https://github.com/youhide/hideOS/blob/main/ARCHITECTURE.md#updates"
        );
        assert_eq!(
            go("../../recipes/system/apps", false).unwrap(),
            "https://github.com/youhide/hideOS/tree/main/recipes/system/apps"
        );
        assert!(go("Nowhere", true).is_err());
        assert!(go("Nowhere.md", false).is_err());
        assert!(go("../../NOWHERE.md", false).is_err());
        assert!(go("../../../outside", false).is_err());
        assert_eq!(links.len(), 4);
    }

    #[test]
    fn a_page_renders_with_ids_and_wrapped_tables() {
        let root = super::super::workspace_root().unwrap();
        let names: BTreeSet<String> = ["Home"].map(String::from).into();
        let page = render(
            "Home",
            "# Title\n\nFirst *words* here.\n\n## A section\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n## A section\n",
            &names,
            &root,
        )
        .unwrap();
        assert_eq!(page.title, "Title");
        assert_eq!(page.summary, "First words here.");
        assert_eq!(
            page.sections,
            vec![
                ("A section".to_owned(), "a-section".to_owned()),
                ("A section".to_owned(), "a-section-1".to_owned())
            ]
        );
        assert!(
            page.body
                .contains("<h2 id=\"a-section\"><a class=\"h\" href=\"#a-section\"")
        );
        assert!(page.body.contains("<div class=\"table-wrap\">\n<table>"));
    }

    #[test]
    fn a_wiki_link_to_a_section_reads_as_the_page() {
        let root = super::super::workspace_root().unwrap();
        let names: BTreeSet<String> = ["Home", "Boot-chain"].map(String::from).into();
        let page = render(
            "Home",
            "# Title\n\nSee [[Boot chain#hidestage]] and [[Boot chain#hidestage|the initrd]].\n",
            &names,
            &root,
        )
        .unwrap();
        assert!(
            page.body
                .contains("<a href=\"Boot-chain.html#hidestage\">Boot chain</a>")
        );
        assert!(
            page.body
                .contains("<a href=\"Boot-chain.html#hidestage\">the initrd</a>")
        );
    }

    #[test]
    fn descriptions_are_cut_at_a_word() {
        assert_eq!(shorten("one two  three", 9), "one two…");
        assert_eq!(shorten("one two", 9), "one two");
    }
}
