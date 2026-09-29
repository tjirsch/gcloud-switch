#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["cmarkgfm>=2025.10.22"]
# ///
"""Render the documentation site: every Markdown page the repo keeps, as
self-contained themed HTML, into one output directory (GitHub Pages).

    uv run scripts/build-site.py [_site]

Pages: README.md → index.html, docs/*.md → docs/<name>.html. Links between the
Markdown files are rewritten to their HTML twins; every page gets the same
navigation bar. The Markdown stays the source GitHub shows — one text, two
renderings. Rendering itself is `build-doc.py`.
"""

import json
import posixpath
import re
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import importlib

doc = importlib.import_module("build-doc")

ROOT = Path(__file__).resolve().parent.parent
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "_site"

# Which `docs/*.md` the site publishes, and which it deliberately does not.
#
# Publishing is a decision, and so is not publishing: a doc must be named in
# exactly one of these two, or the build fails naming it. That way a new doc
# cannot slip onto the public site unnoticed, and cannot be silently left off it
# either.
SITE_DOCS: list[str] = [
    "development",
]

# Not published, and why. These stay in the repository and stay linkable — a link
# to one from a published page is rewritten to GitHub rather than left dead.
SITE_DOCS_EXCLUDED: dict[str, str] = {}

_docs = {md.stem for md in (ROOT / "docs").glob("*.md")}
_unclassified = sorted(_docs - set(SITE_DOCS) - set(SITE_DOCS_EXCLUDED))
_missing = sorted((set(SITE_DOCS) | set(SITE_DOCS_EXCLUDED)) - _docs)
if _unclassified or _missing:
    lines = ["build-site: every docs/*.md must be published or excluded, explicitly."]
    for stem in _unclassified:
        lines.append(f"  docs/{stem}.md is in neither SITE_DOCS nor SITE_DOCS_EXCLUDED")
    for stem in _missing:
        lines.append(f"  docs/{stem}.md is listed but does not exist")
    raise SystemExit("\n".join(lines))

PAGES: list[tuple[Path, str, str]] = []  # (source md, output relative path, nav label)
PAGES.append((ROOT / "README.md", "index.html", "gcloud-switch"))
for stem in SITE_DOCS:
    PAGES.append((ROOT / "docs" / f"{stem}.md", f"docs/{stem}.html", stem))

# The menu, in reading order: what gcloud-switch is and how it is used, then how
# it is built. Every page is named here and nothing else is: an unlisted page is
# a failure, like an unclassified doc above, rather than a menu entry appended
# alphabetically.
NAV_ORDER = [
    "gcloud-switch",
    "development",
]

_labels = {label for _, _, label in PAGES}
if _labels != set(NAV_ORDER):
    lines = ["build-site: the navigation names every page, in a decided order."]
    for label in sorted(_labels - set(NAV_ORDER)):
        lines.append(f"  page {label!r} is not in NAV_ORDER")
    for label in sorted(set(NAV_ORDER) - _labels):
        lines.append(f"  NAV_ORDER names {label!r}, which is not a page")
    raise SystemExit("\n".join(lines))

GITHUB_REPO = "https://github.com/tjirsch/gcloud-switch"
GITHUB_BLOB = f"{GITHUB_REPO}/blob/main/"


def nav_html(current_rel: str) -> str:
    depth = current_rel.count("/")
    up = "../" * depth
    items = []
    by_label = {label: rel for _, rel, label in PAGES}
    for label in NAV_ORDER:
        rel = by_label[label]
        cls = ' class="current"' if rel == current_rel else ""
        items.append(f'<a{cls} href="{up}{rel}">{label}</a>')
    items.append(f'<a href="{GITHUB_REPO}">GitHub</a>')
    return (
        f'<header class="site" data-root="{up}"><nav>'
        + " ".join(items)
        + '</nav><div class="search"><input id="docs-search" type="search" placeholder="Search the docs…" '
        + 'autocomplete="off" spellcheck="false" aria-label="Search the docs">'
        + '<div id="docs-search-results" hidden></div></div></header>\n'
        + f'<script defer src="{up}search-index.js"></script>\n'
    )


TOC_CSS = """
  /* Long pages are the point — the reference is meant to be read straight
     through — so the answer to navigating them is a contents column beside the
     text, not shorter pages. */
  /* The text column is 74ch plus half of the page's width beyond the contents
     column, the gap and 74ch, so the empty space on each side is half what a fixed
     74ch left (9.25rem is half of the contents column and the gap). */
  .page { display: grid; grid-template-columns: 15.5rem minmax(0, calc(50% + 37ch - 9.25rem)); gap: 0 3rem;
    justify-content: center; align-items: start; }
  .page > main { margin: 0; max-width: none; }
  details.toc { position: sticky; top: 4.4rem; margin: 84px 0 0; font-size: .9rem;
    max-height: calc(100vh - 6rem); overflow-y: auto; overscroll-behavior: contain; }
  details.toc summary { font-weight: 600; color: var(--ink-2); cursor: pointer; margin-bottom: .6rem;
    list-style: none; }
  details.toc summary::-webkit-details-marker { display: none; }
  details.toc summary::before { content: "▾ "; color: var(--muted); }
  details.toc:not([open]) summary::before { content: "▸ "; }
  details.toc nav { display: flex; flex-direction: column; border-left: 1px solid var(--line); }
  details.toc a { color: var(--ink-2); text-decoration: none; line-height: 1.35;
    padding: .18rem 0 .18rem .8rem; border-left: 2px solid transparent; margin-left: -1px; }
  details.toc a:hover { color: var(--accent); }
  details.toc a.active { color: var(--accent); border-left-color: var(--accent); }
  details.toc a.lvl3 { padding-left: 1.7rem; font-size: .92em; color: var(--muted); }
  details.toc a.lvl3:hover, details.toc a.lvl3.active { color: var(--accent); }
  /* Narrow: the contents become a collapsed block above the text (the script
     closes it on load), so a long list never buries the page it describes. */
  @media (max-width: 1180px) {
    .page { grid-template-columns: minmax(0, calc(50% + 37ch)); }
    details.toc { position: static; max-height: none; overflow: visible; margin: 28px 0 0; }
  }
  @media print { details.toc { display: none; } }
"""

TOC_JS = r"""
/* The contents column: closed on narrow screens, and marking the section the
   reader is actually in. No dependencies — the site ships no third-party JS. */
(function () {
  var toc = document.querySelector("details.toc");
  if (!toc) return;
  var narrow = window.matchMedia("(max-width: 1180px)");
  if (narrow.matches) toc.open = false;
  var links = [], heads = [];
  Array.prototype.forEach.call(toc.querySelectorAll("a[href^='#']"), function (a) {
    var el = document.getElementById(decodeURIComponent(a.getAttribute("href").slice(1)));
    if (el) { links.push(a); heads.push(el); }
  });
  if (!heads.length) return;
  var tops = [], active = null, queued = false;
  function measure() {
    tops = heads.map(function (el) { return el.getBoundingClientRect().top + window.scrollY; });
  }
  function update() {
    queued = false;
    /* the heading the reader has most recently passed, allowing for the sticky header */
    var y = window.scrollY + 140, i = 0;
    for (var k = 0; k < tops.length; k++) { if (tops[k] <= y) i = k; else break; }
    var a = links[i];
    if (a === active) return;
    if (active) active.classList.remove("active");
    active = a;
    a.classList.add("active");
    /* keep the mark visible in a long contents list, without scrolling the page:
       only the aside's own scrollTop is touched */
    if (!narrow.matches && toc.open && toc.scrollHeight > toc.clientHeight) {
      var r = a.getBoundingClientRect(), t = toc.getBoundingClientRect();
      if (r.top < t.top) toc.scrollTop -= (t.top - r.top) + 8;
      else if (r.bottom > t.bottom) toc.scrollTop += (r.bottom - t.bottom) + 8;
    }
  }
  function onScroll() { if (!queued) { queued = true; requestAnimationFrame(update); } }
  measure();
  update();
  window.addEventListener("scroll", onScroll, { passive: true });
  window.addEventListener("resize", function () { measure(); update(); }, { passive: true });
  /* fonts and inlined SVGs change the offsets after first paint */
  window.addEventListener("load", function () { measure(); update(); });
})();
"""

NAV_CSS = """
  header.site { position: sticky; top: 0; z-index: 10; background: var(--surface); border-bottom: 1px solid var(--line);
    display: flex; flex-wrap: wrap; align-items: center; gap: .4rem 1.25rem; padding: .55rem 1.5rem; }
  header.site nav { display: flex; flex-wrap: wrap; align-items: center; gap: .3rem 1.1rem; font-size: .95rem; font-weight: 500; }
  header.site nav a { color: var(--ink-2); text-decoration: none; padding: .15rem 0; border-bottom: 2px solid transparent; }
  header.site nav a:hover { color: var(--accent); }
  header.site nav a.current { color: var(--accent); border-bottom-color: var(--accent); }
  header.site .search { position: relative; margin-left: auto; flex: 0 1 22rem; min-width: 14rem; }
  #docs-search { width: 100%; font: inherit; font-size: .95rem; color: var(--ink); background: var(--ground);
    border: 1px solid var(--line); border-radius: .5rem; padding: .4rem .75rem; outline: none; }
  #docs-search:focus { border-color: var(--accent); background: var(--surface); }
  #docs-search-results { position: absolute; right: 0; left: 0; top: calc(100% + .35rem); max-height: 24rem; overflow-y: auto;
    background: var(--surface); border: 1px solid var(--line); border-radius: .5rem; box-shadow: 0 8px 28px rgba(0,0,0,.18); }
  #docs-search-results a { display: block; padding: .5rem .75rem; text-decoration: none; border-bottom: 1px solid var(--line); }
  #docs-search-results a:last-child { border-bottom: none; }
  #docs-search-results a.sel, #docs-search-results a:hover { background: var(--band); }
  #docs-search-results .where { color: var(--muted); font-size: .78rem; }
  #docs-search-results .head { color: var(--ink); font-weight: 600; font-size: .9rem; }
  #docs-search-results .head mark, #docs-search-results .snip mark { background: var(--accent-soft); color: var(--accent); }
  #docs-search-results .snip { color: var(--ink-2); font-size: .82rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  #docs-search-results .none { padding: .5rem .75rem; color: var(--muted); font-size: .85rem; }
"""

SEARCH_JS = """
(function () {
  var input = document.getElementById('docs-search');
  var box = document.getElementById('docs-search-results');
  if (!input || !box || !window.DOCS_INDEX) return;
  var root = (document.querySelector('header.site') || {}).getAttribute
    ? (document.querySelector('header.site').getAttribute('data-root') || '') : '';
  var sel = -1, hits = [];
  function esc(s) { return s.replace(/[&<>"]/g, function (c) { return ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'})[c]; }); }
  function mark(s, terms) {
    var e = esc(s);
    terms.forEach(function (t) {
      if (!t) return;
      e = e.replace(new RegExp('(' + t.replace(/[.*+?^${}()|[\\]\\\\]/g, '\\\\$&') + ')', 'ig'), '<mark>$1</mark>');
    });
    return e;
  }
  function search(q) {
    var terms = q.toLowerCase().split(/\\s+/).filter(Boolean);
    if (!terms.length) return [];
    return window.DOCS_INDEX.map(function (e) {
      var h = e.h.toLowerCase(), x = e.x.toLowerCase(), t = e.t.toLowerCase();
      var ok = terms.every(function (w) { return h.indexOf(w) >= 0 || x.indexOf(w) >= 0 || t.indexOf(w) >= 0; });
      if (!ok) return null;
      var score = terms.reduce(function (s, w) {
        if (h.indexOf(w) === 0) return s + 3;
        if (h.indexOf(w) >= 0) return s + 2;
        return s + 1;
      }, e.a ? 0 : 1);
      return { e: e, score: score };
    }).filter(Boolean).sort(function (a, b) { return b.score - a.score; }).slice(0, 12).map(function (r) { return r.e; });
  }
  function render(q) {
    hits = search(q); sel = -1;
    if (!q.trim()) { box.hidden = true; box.innerHTML = ''; return; }
    var terms = q.toLowerCase().split(/\\s+/).filter(Boolean);
    box.innerHTML = hits.length
      ? hits.map(function (e) {
          return '<a href="' + root + e.p + (e.a ? '#' + e.a : '') + '">'
            + '<div class="where">' + esc(e.t) + '</div>'
            + '<div class="head">' + mark(e.h, terms) + '</div>'
            + (e.x ? '<div class="snip">' + mark(e.x, terms) + '</div>' : '')
            + '</a>';
        }).join('')
      : '<div class="none">Nothing found.</div>';
    box.hidden = false;
  }
  function move(d) {
    var links = box.querySelectorAll('a');
    if (!links.length) return;
    sel = (sel + d + links.length) % links.length;
    links.forEach(function (l, i) { l.classList.toggle('sel', i === sel); });
    links[sel].scrollIntoView({ block: 'nearest' });
  }
  input.addEventListener('input', function () { render(input.value); });
  input.addEventListener('keydown', function (ev) {
    if (ev.key === 'ArrowDown') { ev.preventDefault(); move(1); }
    else if (ev.key === 'ArrowUp') { ev.preventDefault(); move(-1); }
    else if (ev.key === 'Enter') {
      var links = box.querySelectorAll('a');
      var l = links[sel >= 0 ? sel : 0];
      if (l) window.location.href = l.getAttribute('href');
    } else if (ev.key === 'Escape') { box.hidden = true; input.blur(); }
  });
  document.addEventListener('click', function (ev) {
    if (!box.contains(ev.target) && ev.target !== input) box.hidden = true;
  });
  document.addEventListener('keydown', function (ev) {
    if (ev.key === '/' && document.activeElement !== input) { ev.preventDefault(); input.focus(); }
  });
})();
"""


def index_entries(title: str, rel: str, body: str) -> list[dict]:
    """One entry per page plus one per h1–h3: heading, anchor and a short
    excerpt of the text that follows — what the search box looks through."""
    entries = []
    parts = re.split(r"(<h[123][^>]*>.*?</h[123]>)", body, flags=re.S)
    lead = doc.plain_text(parts[0])[:180]
    entries.append({"t": title, "p": rel, "a": "", "h": title, "x": lead})
    for i in range(1, len(parts), 2):
        m = re.match(
            r"<h([123])[^>]*?id=\"([^\"]+)\"[^>]*>(.*?)</h\1>", parts[i], flags=re.S
        )
        if not m:
            continue
        heading = doc.plain_text(m.group(3))
        follow = doc.plain_text(parts[i + 1] if i + 1 < len(parts) else "")[:180]
        entries.append(
            {"t": title, "p": rel, "a": m.group(2), "h": heading, "x": follow}
        )
    return entries


def command_anchors(body: str) -> str:
    """A heading that names a command in backticks — `### Activate a profile (`activate`)` —
    gets a stable `id="cmd-activate"` so `gcloud-switch <cmd> --html-help` can open it.
    The rendered heading keeps its own generated id as a nested anchor."""

    def repl(m: "re.Match[str]") -> str:
        level, attrs, slug, inner = m.group(1), m.group(2), m.group(3), m.group(4)
        cmds = re.findall(r"<code>([a-z][a-z0-9-]*)</code>", inner)
        if not cmds:
            return m.group(0)
        keep = f'<a id="{slug}"></a>' if slug else ""
        return f'<h{level}{attrs} id="cmd-{cmds[0]}">{keep}{inner}</h{level}>'

    return re.sub(
        r'<h([23])((?:\s+(?!id=)[a-z-]+="[^"]*")*)(?:\s+id="([^"]*)")?>(.*?\(<code>[a-z][a-z0-9-]*</code>\).*?)</h\1>',
        repl,
        body,
    )


TOC_MIN_HEADINGS = 3


def toc_html(body: str) -> str:
    """ "On this page" — the h2/h3 headings of the rendered body, as a sidebar.

    Built AFTER `command_anchors` has run, so the hrefs are the ids the document
    actually carries. h4 and deeper are left out: a table of contents that lists
    every paragraph is another long page to navigate.
    """
    heads = re.findall(
        r'<h([23])[^>]*?\bid="([^"]+)"[^>]*>(.*?)</h\1>', body, flags=re.S
    )
    if len(heads) < TOC_MIN_HEADINGS:
        return ""
    items = []
    for level, anchor, inner in heads:
        label = html_escape(doc.plain_text(inner))
        items.append(f'<a class="lvl{level}" href="#{anchor}">{label}</a>')
    return (
        '<details class="toc" open><summary>On this page</summary><nav>'
        + "".join(items)
        + "</nav></details>\n"
    )


def html_escape(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def rewrite_links(body: str, src_rel: Path) -> str:
    """`docs/x.md` / `../README.md` / `#anchor` links → the rendered twins.

    A link to a repository file the site does not publish (an excluded doc, an
    ADR) is sent to GitHub rather than left as a `.md` href that 404s: the file
    is still there, it is simply not a page. A link to a file that does not exist,
    or that leaves the repository, fails the build naming it. Paths resolve
    against the repository, never the working directory.
    """
    targets = {str(src.relative_to(ROOT)): rel for src, rel, _ in PAGES}
    broken: list[str] = []

    def repl(m: "re.Match[str]") -> str:
        href = m.group(1)
        if href.startswith(("http://", "https://", "#", "mailto:")):
            return m.group(0)
        path, _, frag = href.partition("#")
        if not path.endswith(".md"):
            return m.group(0)
        anchor = f"#{frag}" if frag else ""
        if path.startswith("/"):
            target = Path(path.lstrip("/"))
        else:
            resolved = (ROOT / src_rel.parent / path).resolve()
            if not resolved.is_relative_to(ROOT.resolve()):
                broken.append(f"  {href} leaves the repository")
                return m.group(0)
            target = resolved.relative_to(ROOT.resolve())
        page = targets.get(str(target))
        if page is None:
            if not (ROOT / target).is_file():
                broken.append(f"  {href} names {target}, which does not exist")
                return m.group(0)
            return f'href="{GITHUB_BLOB}{target.as_posix()}{anchor}"'
        here = Path(targets[str(src_rel)]).parent
        rel = Path(*([".."] * len(here.parts))) / page if here.parts else Path(page)
        return f'href="{rel.as_posix()}{anchor}"'

    body = re.sub(r'href="([^"]+)"', repl, body)
    if broken:
        raise SystemExit(
            f"build-site: {src_rel} links to nothing:\n" + "\n".join(broken)
        )
    return body


def dead_anchors(pages: dict[str, str]) -> list[str]:
    """Every `page.html#anchor` link between pages of the site whose target page
    carries no such id. Heading ids are GitHub's, so an anchor that lands here
    lands on GitHub too, and one that does not is dead in both."""
    ids = {rel: set(re.findall(r'\bid="([^"]+)"', page)) for rel, page in pages.items()}
    dead = []
    for rel, page in pages.items():
        here = posixpath.dirname(rel)
        for target, frag in re.findall(r'href="([^"#:]*)#([^"]+)"', page):
            to = posixpath.normpath(posixpath.join(here, target)) if target else rel
            if to in ids and frag not in ids[to]:
                dead.append(f"  {rel}: #{frag} is not an anchor of {to}")
    return dead


def main() -> None:
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    index: list[dict] = []
    pages: dict[str, str] = {}
    for src, rel, _label in PAGES:
        doc.MD = src  # the renderer inlines SVGs relative to the source
        body = doc.render(src.read_text(encoding="utf-8"))
        title = doc.page_title(body, src.relative_to(ROOT))
        body = rewrite_links(body, src.relative_to(ROOT))
        body = command_anchors(body)
        index.extend(index_entries(title, rel, body))
        pages[rel] = (
            doc.head(title)
            + "<style>"
            + doc.CSS
            + NAV_CSS
            + TOC_CSS
            + "</style>\n"
            + nav_html(rel)
            + '<div class="page">\n'
            + toc_html(body)
            + "<main>\n"
            + body
            + "\n</main>\n</div>\n"
        )
    dead = dead_anchors(pages)
    if dead:
        raise SystemExit(
            "build-site: links to anchors that do not exist:\n" + "\n".join(dead)
        )
    for rel, page in pages.items():
        out = OUT / rel
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(page, encoding="utf-8")
        print(f"wrote {out.relative_to(OUT)} ({out.stat().st_size} bytes)")

    (OUT / "search-index.js").write_text(
        "window.DOCS_INDEX="
        + json.dumps(index, ensure_ascii=False)
        + ";\n"
        + SEARCH_JS
        + TOC_JS,
        encoding="utf-8",
    )
    print(
        f"wrote search-index.js ({(OUT / 'search-index.js').stat().st_size} bytes, {len(index)} entries)"
    )
    (OUT / ".nojekyll").write_text("")


if __name__ == "__main__":
    main()
