#!/usr/bin/env python3
"""Materialize crawler-visible metadata, static navigation and the sitemap
for iphone-use.leeguoo.com (adapted from chrome-use's docs/_tools/seo.py).

Run from any directory:  python3 site/_tools/seo.py          (write)
                         python3 site/_tools/seo.py --check  (fail if stale or invalid)

Everything this script writes lives between `<!-- seo:NAME -->` markers (plus
the static sidebar inside `<nav id="du-sidebar">`), so a re-run is byte-stable.
The page list and labels come from NAV in site/public/assets/docs/docs.js, the
same source the runtime sidebar uses.

URLs are extensionless (/install, /en/install): the Cloudflare Worker serves
install.html there (html_handling = "auto-trailing-slash") and redirects the
.html form, so canonicals, hreflang, the sitemap and every internal link use
the extensionless form. A link ending in .html is reported as a problem.
"""
from pathlib import Path
import datetime, html, json, re, subprocess, sys, unicodedata
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]          # repository root (git commands)
DOCS = ROOT / 'site' / 'public'                      # what the Worker serves
ORIGIN = 'https://iphone-use.leeguoo.com'
OG_IMAGE = ORIGIN + '/assets/og.png'
GH = 'https://github.com/leeguooooo/iphone-use'
PERSON = 'https://leeguoo.com/about#person'
LICENSE_URL = 'https://polyformproject.org/licenses/shield/1.0.0'
NS = 'http://www.sitemaps.org/schemas/sitemap/0.9'
ALT = 'http://www.w3.org/1999/xhtml'
ET.register_namespace('', NS)
ET.register_namespace('xhtml', ALT)
check = '--check' in sys.argv
changes, problems = [], []

# ------------------------------------------------------------ NAV from docs.js
js = (DOCS / 'assets/docs/docs.js').read_text(encoding='utf-8')
nav_src = js[js.index('var NAV = ['):js.index('var TABS')]
NAV = []
for m in re.finditer(r'\{\s*id:\s*"(\w+)",\s*label:\s*\{\s*zh:\s*"([^"]+)",\s*en:\s*"([^"]+)"\s*\}|'
                     r'\{\s*slug:\s*"([\w-]+)",\s*ico:\s*"([^"]+)",\s*zh:\s*"([^"]+)",\s*en:\s*"([^"]+)"', nav_src):
    if m[1]:
        NAV.append({'id': m[1], 'zh': m[2], 'en': m[3], 'items': []})
    else:
        NAV[-1]['items'].append({'slug': m[4], 'ico': m[5], 'zh': m[6], 'en': m[7]})
GROUP_OF = {it['slug']: g for g in NAV for it in g['items']}
ITEM = {it['slug']: it for g in NAV for it in g['items']}
if not ITEM:
    problems.append('no NAV items parsed from docs.js (keep the slug, ico, zh, en key order)')
OUT = {
    'zh': [(GH, 'GitHub'), (GH + '/releases', '下载 / Releases'),
           ('https://github.com/leeguooooo/iphone-use-flows', '官方 flow 源'),
           ('https://blog.leeguoo.com/zh/posts/iphone-use/', '博客：iphone-use 产品介绍'),
           ('https://blog.leeguoo.com/zh/posts/iphone-use-video/', '博客：演示视频'),
           ('https://chrome-use.leeguoo.com/', 'chrome-use（让 agent 用你的 Chrome）'),
           ('https://leeguoo.com/about', '作者')],
    'en': [(GH, 'GitHub'), (GH + '/releases', 'Releases'),
           ('https://github.com/leeguooooo/iphone-use-flows', 'Official flow registry'),
           ('https://blog.leeguoo.com/en/posts/iphone-use/', 'Blog: iphone-use guide'),
           ('https://blog.leeguoo.com/en/posts/iphone-use-video/', 'Blog: demo video'),
           ('https://chrome-use.leeguoo.com/en/', 'chrome-use (agents in your Chrome)'),
           ('https://leeguoo.com/about/en', 'Author')],
}
SOFTWARE_VERSION = json.loads((ROOT / 'server.json').read_text(encoding='utf-8'))['version']


def route(slug, en):
    return ('/en/' if en else '/') + ('' if slug in ('index', 'overview') else slug)


def esc(s):
    return html.escape(s, quote=True)


def text_of(fragment):
    return re.sub(r'\s+', ' ', html.unescape(re.sub(r'<[^>]+>', '', fragment))).strip()


def width(s):
    # Search snippets truncate by pixel width: count wide (CJK) characters twice.
    return sum(2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in s)


def is_en(src):
    return bool(re.search(r'<html[^>]+lang=["\']en', src))


def noindex(src):
    return bool(re.search(r'<meta[^>]+name=["\']robots["\'][^>]+noindex', src))


def sidebar(slug, en):
    lng = 'en' if en else 'zh'
    out = []
    for g in NAV:
        out.append(f'<div class="du-nav-group"><div class="du-nav-label">{esc(g[lng])}</div>')
        for it in g['items']:
            cur = ' active" aria-current="page' if it['slug'] == slug else ''
            out.append(f'<a class="du-nav-item{cur}" href="{route(it["slug"], en)}"><span class="du-nav-ico">{it["ico"]}</span>'
                       f'<span class="du-nav-text">{esc(it[lng])}</span></a>')
        out.append('</div>')
    return ''.join(out)


def footer(en, mirror_href):
    lng = 'en' if en else 'zh'
    links = [f'<a href="{route(it["slug"], en)}">{esc(it[lng])}</a>' for g in NAV for it in g['items']]
    links += [f'<a href="{h}">{esc(t)}</a>' for h, t in OUT[lng]]
    if mirror_href:
        links.append(f'<a href="{mirror_href}" hreflang="{"zh-Hans" if en else "en"}">{"中文" if en else "English"}</a>')
    label = 'Site' if en else '站点'
    return (f'<!-- seo:footer --><nav class="du-static-footer" aria-label="{label}">'
            f'<a href="{route("index", en)}">iphone-use</a> · ' + ' · '.join(links) + '</nav><!-- /seo:footer -->')


FONTS_PLACEHOLDER = '<link href="https://fonts.googleapis.com/css2?family=" rel="stylesheet" />'


def strip_managed(s):
    s = re.sub(r'<!-- seo:fonts -->.*?<!-- /seo:fonts -->', FONTS_PLACEHOLDER, s, flags=re.S)
    s = re.sub(r'<link href="https://fonts\.googleapis\.com/css2\?[^"]*" rel="stylesheet" />', FONTS_PLACEHOLDER, s)
    s = re.sub(r'\s*<!-- seo:(\w+) -->.*?<!-- /seo:\1 -->', '', s, flags=re.S)
    s = re.sub(r'\s*<link\b[^>]*\brel=["\'](?:canonical|alternate)["\'][^>]*>', '', s)
    s = re.sub(r'<nav id="du-sidebar"[^>]*>.*?</nav>', '<nav id="du-sidebar"></nav>', s, flags=re.S)
    s = s.replace('<div id="du-topbar" class="du-topbar"></div>', '<div id="du-topbar"></div>')
    return s


def git_date(p):
    # Last content commit, ignoring metadata-only "site(seo)" passes so a site-wide
    # regeneration does not bump every page's lastmod.
    return subprocess.check_output(['git', 'log', '-1', '--format=%cs', '--invert-grep', '--grep=^site(seo)', '--', str(p)],
                                   cwd=ROOT, text=True).strip()


def content_date(p, source):
    rel = p.relative_to(ROOT).as_posix()
    try:
        head = subprocess.check_output(['git', 'show', f'HEAD:{rel}'], cwd=ROOT, text=True, stderr=subprocess.DEVNULL)
    except subprocess.CalledProcessError:
        return datetime.date.today().isoformat()
    if strip_managed(head) != strip_managed(source):
        return datetime.date.today().isoformat()
    return git_date(p) or datetime.date.today().isoformat()


def faq_items(clean):
    """FAQPage entities from <section class="du-faq">: each <h3> is a question,
    everything up to the next <h3> (or the end of the section) is its answer."""
    items = []
    for sec in re.findall(r'<section class="du-faq"[^>]*>(.*?)</section>', clean, re.S):
        parts = re.split(r'<h3\b[^>]*>(.*?)</h3>', sec, flags=re.S)
        for q, a in zip(parts[1::2], parts[2::2]):
            q, a = text_of(q), text_of(a)
            if q and a:
                items.append({'@type': 'Question', 'name': q, 'acceptedAnswer': {'@type': 'Answer', 'text': a}})
    return items


def software_app(en, canonical, desc):
    return {
        '@context': 'https://schema.org', '@type': 'SoftwareApplication', '@id': ORIGIN + '/#software',
        'name': 'iphone-use', 'alternateName': 'iphone-use by leeguoo',
        'applicationCategory': 'DeveloperApplication',
        'applicationSubCategory': 'iPhone automation for AI agents (MCP server, HTTP API, CLI)',
        'operatingSystem': 'macOS, iOS',
        'description': desc, 'url': canonical, 'inLanguage': 'en' if en else 'zh-Hans',
        'downloadUrl': GH + '/releases', 'installUrl': ORIGIN + route('install', en),
        'softwareVersion': SOFTWARE_VERSION,
        'offers': {'@type': 'Offer', 'price': '0', 'priceCurrency': 'USD'},
        'isAccessibleForFree': True,
        'license': LICENSE_URL,
        'image': OG_IMAGE, 'screenshot': OG_IMAGE,
        'author': {'@id': PERSON}, 'creator': {'@id': PERSON},
        'codeRepository': GH,
        'sameAs': [GH, 'https://www.producthunt.com/products/iphone-use', 'https://glama.ai/mcp/servers/leeguooooo/iphone-use'],
    }


PERSON_LD = {'@context': 'https://schema.org', '@type': 'Person', '@id': PERSON, 'name': '郭立',
             'alternateName': ['Guo Li', 'Leo', 'leeguoo'], 'url': 'https://leeguoo.com/about',
             'sameAs': ['https://github.com/leeguooooo', 'https://blog.leeguoo.com', 'https://x.com/leeguooooo']}

urls = ET.Element(f'{{{NS}}}urlset')
seen = set()
pages = sorted([*DOCS.glob('*.html'), *DOCS.glob('en/*.html')])
for slug_ in ITEM:
    for en in (False, True):
        f = DOCS / (('en/' if en else '') + ('index' if slug_ == 'overview' else slug_) + '.html')
        if not f.exists():
            problems.append(f'NAV slug {slug_} has no {"en" if en else "zh"} page')

titles, descs = {}, {}
for p in pages:
    rel = p.relative_to(DOCS).as_posix()
    slug = p.stem
    en = rel.startswith('en/')
    source = p.read_text(encoding='utf-8')
    actual_en = is_en(source)
    if actual_en != en:
        problems.append(f'{rel}: <html lang> does not match its directory')
    if not actual_en and not re.search(r'<html[^>]+lang="zh-CN"', source):
        problems.append(f'{rel}: zh pages must use <html lang="zh-CN">')
    canonical = ORIGIN + route(slug, en)
    mirror = DOCS / (slug + '.html') if en else DOCS / 'en' / (slug + '.html')
    hidden = noindex(source)
    is404 = slug == '404'
    if is404 and not hidden:
        problems.append(f'{rel}: 404 pages must be noindex')
    if hidden and not is404:
        problems.append(f'{rel}: only 404 pages may be noindex')
    paired = mirror.exists() and not hidden
    if not hidden and not mirror.exists():
        problems.append(f'{rel}: no {"zh" if en else "en"} mirror')
    # x-default -> English: zh readers are routed by zh-Hans; other locales get English.
    alts = [('zh-Hans', False), ('en', True), ('x-default', True)] if paired else []
    page_slug = (re.search(r'data-page="([\w-]+)"', source) or [None, slug])[1]
    lng = 'en' if actual_en else 'zh'
    if not hidden and page_slug not in ITEM:
        problems.append(f'{rel}: data-page "{page_slug}" is not in docs.js NAV')

    clean = strip_managed(source)
    title = text_of(re.search(r'<title>(.*?)</title>', clean, re.S)[1])
    dm = re.search(r'<meta\s+name="description"\s+content="([^"]*)"', clean)
    desc = html.unescape(dm[1]) if dm else ''
    if not hidden:
        if not desc:
            problems.append(f'{rel}: missing meta description')
        elif width(desc) > 160:
            problems.append(f'{rel}: description is {width(desc)} wide (max 160; CJK counts 2)')
        if 'iphone-use' not in title:
            problems.append(f'{rel}: title lacks "iphone-use"')
        if width(title) > 60:
            problems.append(f'{rel}: title is {width(title)} wide (max 60; CJK counts 2)')
        if title in titles:
            problems.append(f'{rel}: title duplicates {titles[title]}')
        if desc in descs:
            problems.append(f'{rel}: description duplicates {descs[desc]}')
        titles[title], descs[desc] = rel, rel
    if len(re.findall(r'<h1\b', clean)) != 1:
        problems.append(f'{rel}: needs exactly one <h1>')
    for href in re.findall(r'href="([^"]+)"', clean):
        if not re.match(r'(?:https?:|mailto:|#)', href):
            if re.search(r'\.html(?:$|[#?])', href):
                problems.append(f'{rel}: internal link {href} uses .html (use the extensionless URL)')
            path = href.split('#')[0].split('?')[0]
            if path.startswith('/') and not path.startswith('/assets/') and path not in ('/favicon-32.png',):
                target = DOCS / (path.lstrip('/') + ('index.html' if path.endswith('/') else '.html'))
                if not target.exists() and not (DOCS / path.lstrip('/')).is_file():
                    problems.append(f'{rel}: broken internal link {href}')

    # ---------------------------------------------------------------- <head>
    head = [] if is404 else [f'<link rel="canonical" href="{canonical}" />']
    for hl, to_en in alts:
        head.append(f'<link rel="alternate" hreflang="{hl}" href="{ORIGIN + route(slug, to_en)}" />')

    def has(prop):
        return re.search(r'<meta[^>]+(?:property|name)="' + re.escape(prop) + '"', clean)
    og = [('og:type', 'website' if slug == 'index' else 'article'),
          ('og:site_name', 'iphone-use'), ('og:title', title), ('og:description', desc), ('og:url', canonical),
          ('og:image', OG_IMAGE), ('og:image:width', '1200'), ('og:image:height', '630'),
          ('og:image:alt', 'iphone-use: let AI agents use your real iPhone' if actual_en else 'iphone-use：让 AI agent 操作你的真实 iPhone'),
          ('og:locale', 'en_US' if actual_en else 'zh_CN')]
    if paired:
        og.append(('og:locale:alternate', 'zh_CN' if actual_en else 'en_US'))
    for k, v in ([] if is404 else og):
        if not has(k) and v:
            head.append(f'<meta property="{k}" content="{esc(v)}" />')
    tw = [('twitter:card', 'summary_large_image'), ('twitter:title', title), ('twitter:description', desc),
          ('twitter:image', OG_IMAGE), ('twitter:image:alt', 'iphone-use: let AI agents use your real iPhone' if actual_en else 'iphone-use：让 AI agent 操作你的真实 iPhone')]
    for k, v in ([] if is404 else tw):
        if not has(k) and v:
            head.append(f'<meta name="{k}" content="{esc(v)}" />')
    ld = []
    if not hidden:
        h1 = re.search(r'<h1\b[^>]*>(.*?)</h1>', clean, re.S)
        if slug == 'index':
            ld += [software_app(actual_en, canonical, desc), PERSON_LD]
        else:
            crumbs = [('iphone-use', ORIGIN + route('index', actual_en))]
            g = GROUP_OF.get(page_slug)
            if g:
                first = g['items'][0]['slug']
                crumbs.append((g[lng], ORIGIN + route(first, actual_en)))
            crumbs.append((text_of(h1[1]) if h1 else title, canonical))
            if len(crumbs) == 3 and crumbs[1][1] in (canonical, crumbs[0][1]):
                crumbs.pop(1)
            ld.append({'@context': 'https://schema.org', '@type': 'BreadcrumbList', 'itemListElement': [
                {'@type': 'ListItem', 'position': i + 1, 'name': n, 'item': u} for i, (n, u) in enumerate(crumbs)]})
            ld.append({'@context': 'https://schema.org', '@type': 'TechArticle',
                       'headline': (text_of(h1[1]) if h1 else title)[:110], 'description': desc,
                       'inLanguage': 'en' if actual_en else 'zh-Hans', 'url': canonical, 'mainEntityOfPage': canonical,
                       'image': OG_IMAGE, 'dateModified': content_date(p, source),
                       'author': {'@id': PERSON, '@type': 'Person', 'name': 'Guo Li (leeguoo)', 'url': 'https://leeguoo.com/about'},
                       'publisher': {'@id': PERSON, '@type': 'Person', 'name': 'Guo Li (leeguoo)', 'url': 'https://leeguoo.com/about'},
                       'isPartOf': {'@type': 'WebSite', 'name': 'iphone-use', 'url': ORIGIN + '/'},
                       'about': {'@id': ORIGIN + '/#software', '@type': 'SoftwareApplication', 'name': 'iphone-use'}})
        faqs = faq_items(clean)
        if faqs:
            ld.append({'@context': 'https://schema.org', '@type': 'FAQPage', 'inLanguage': 'en' if actual_en else 'zh-Hans',
                       'url': canonical, 'mainEntity': faqs})
        if slug in ('index', 'faq') and not faqs:
            problems.append(f'{rel}: expected a <section class="du-faq"> for FAQPage')
    for obj in ld:
        head.append('<script type="application/ld+json">' + json.dumps(obj, ensure_ascii=False, separators=(',', ':')) + '</script>')
    block = '<!-- seo:head -->\n  ' + '\n  '.join(head) + '\n  <!-- /seo:head -->'
    result = clean.replace('</head>', '  ' + block + '\n</head>', 1) if head else clean

    # ---------------------------------------------------------------- fonts
    # Display fonts only (Google serves CJK fonts in unicode-range slices, so /en/
    # pages fetch only the Latin slice); body text uses
    # system fonts, so no CJK body web font is ever loaded. Non-blocking, swap.
    # /en/ requests ZCOOL KuaiLe only for printable ASCII (one small file via the
    # css2 `text=` parameter), so no CJK font slice is ever fetched there.
    from urllib.parse import quote
    if actual_en:
        ascii_text = quote(''.join(chr(c) for c in range(32, 127)), safe='')
        hrefs = ['https://fonts.googleapis.com/css2?family=Permanent+Marker&family=JetBrains+Mono:wght@400;700&display=swap',
                 'https://fonts.googleapis.com/css2?family=ZCOOL+KuaiLe&display=swap&text=' + ascii_text]
    else:
        hrefs = ['https://fonts.googleapis.com/css2?family=ZCOOL+KuaiLe&family=Permanent+Marker&family=JetBrains+Mono:wght@400;700&display=swap']
    fonts = '<!-- seo:fonts -->' + ''.join(
        f'<link rel="preload" as="style" href="{esc(h)}" onload="this.onload=null;this.rel=\'stylesheet\'" />'
        f'<noscript><link rel="stylesheet" href="{esc(h)}" /></noscript>' for h in hrefs) + '<!-- /seo:fonts -->'
    result = result.replace(FONTS_PLACEHOLDER, fonts, 1)

    # ---------------------------------------------------------------- static navigation
    result = result.replace('<div id="du-topbar"></div>', '<div id="du-topbar" class="du-topbar"></div>')
    result = re.sub(r'<nav id="du-sidebar"[^>]*></nav>',
                    lambda m: f'<nav id="du-sidebar" class="du-sidebar" aria-label="{"Documentation" if actual_en else "文档"}">'
                              + sidebar(page_slug, actual_en) + '</nav>', result, count=1)
    mirror_href = route(slug, not en) if paired else None
    foot = footer(actual_en, mirror_href)
    if '</main>' in result:
        result = re.sub(r'\s*</main>', lambda m: foot + '\n    </main>', result, count=1)
    else:
        result = result.replace('</body>', foot + '\n</body>', 1)

    for blob in re.findall(r'<script type="application/ld\+json">(.*?)</script>', result, re.S):
        try:
            json.loads(blob)
        except ValueError as e:
            problems.append(f'{rel}: invalid JSON-LD ({e})')

    if result != source:
        if check:
            norm = lambda s: re.sub(r'"dateModified":"[\d-]+"', '', s)
            if norm(result) != norm(source):
                changes.append(rel)
        else:
            changes.append(rel)
            p.write_text(result, encoding='utf-8')
    if hidden or canonical in seen:
        continue
    seen.add(canonical)
    item = ET.SubElement(urls, f'{{{NS}}}url')
    ET.SubElement(item, f'{{{NS}}}loc').text = canonical
    for hl, to_en in alts:
        ET.SubElement(item, f'{{{ALT}}}link', {'rel': 'alternate', 'hreflang': hl, 'href': ORIGIN + route(slug, to_en)})
    date = content_date(p, source)
    if date:
        ET.SubElement(item, f'{{{NS}}}lastmod').text = date

ET.indent(urls, space='  ')
sitemap = '<?xml version="1.0" encoding="UTF-8"?>\n' + ET.tostring(urls, encoding='unicode') + '\n'
robots = DOCS / 'robots.txt'
if not robots.exists() or f'Sitemap: {ORIGIN}/sitemap.xml' not in robots.read_text(encoding='utf-8'):
    problems.append('robots.txt must name the sitemap')
if check:
    # lastmod may change after commits: validate the URL set rather than demand churn.
    if not (DOCS / 'sitemap.xml').exists():
        changes.append('sitemap.xml missing')
    else:
        existing = ET.parse(DOCS / 'sitemap.xml')
        if set(x.text for x in existing.findall('.//{*}loc')) != set(x.text for x in urls.findall('.//{*}loc')):
            changes.append('sitemap URL coverage')

        def alternates(tree):
            return {e.find('{*}loc').text: {(l.get('hreflang'), l.get('href')) for l in e.findall('{*}link')} for e in tree.findall('.//{*}url')}
        if alternates(existing) != alternates(urls):
            changes.append('sitemap language alternates')
else:
    (DOCS / 'sitemap.xml').write_text(sitemap, encoding='utf-8')
if problems:
    raise SystemExit('SEO problems:\n  ' + '\n  '.join(problems))
if check and changes:
    raise SystemExit('Stale SEO metadata (run python3 site/_tools/seo.py): ' + ', '.join(changes))
print(f'SEO metadata verified for {len(pages)} pages' if check else f'Materialized SEO metadata for {len(pages)} pages ({len(changes)} changed)')
