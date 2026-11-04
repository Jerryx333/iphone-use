# iphone-use.leeguoo.com

The public website for iphone-use: static HTML, served by a Cloudflare Worker with static assets
(`iphone-use-site`, custom domain `iphone-use.leeguoo.com`). No build step.

```text
site/
  wrangler.toml          Worker config (assets only, no script)
  public/                everything the Worker serves
    index.html …         Chinese pages at the root
    en/…                 English pages, same file names
    404.html, en/404.html  not-found pages (noindex)
    assets/docs/         design system shared with the chrome-use docs (docs.css, docs.js)
    assets/og.png        1200×630 social card
    robots.txt, llms.txt, sitemap.xml
  _tools/seo.py          static nav + metadata + JSON-LD + sitemap generator
  _tools/og.html         source of assets/og.png
```

## Editing pages

Each page is a plain HTML file. Edit the `<title>`, `<meta name="description">`, `<body data-page>` and
the content of `<main>`; leave everything between `<!-- seo:… -->` markers and the contents of
`<nav id="du-sidebar">` alone, `seo.py` rewrites them. Keep zh (`public/<slug>.html`) and en
(`public/en/<slug>.html`) in step.

- URLs are extensionless: link to `/install` and `/en/install`, never `install.html` (the Worker
  serves `install.html` at `/install` and redirects the `.html` form).
- A new page needs an entry in `NAV` in `public/assets/docs/docs.js` (keep the `slug, ico, zh, en`
  key order; `seo.py` parses it).
- Questions inside `<section class="du-faq">` (each an `<h3>` followed by its answer) become
  `FAQPage` JSON-LD.

## Regenerate metadata

```bash
python3 site/_tools/seo.py          # write: head tags, JSON-LD, static sidebar/footer, fonts, sitemap.xml
python3 site/_tools/seo.py --check  # verify only; fails on stale output or a problem
```

`seo.py` writes canonical + `hreflang` (zh-Hans / en / x-default → `/en/`), Open Graph and Twitter
tags, `SoftwareApplication` + `Person` on the home pages, `TechArticle` + `BreadcrumbList` on the
others, `FAQPage` where there is a `du-faq` section, the full static sidebar and footer, and
`sitemap.xml` with `lastmod` from git (commits whose subject starts with `site(seo)` do not count).
It fails on: a title wider than 60 or a description wider than 160 (CJK characters count 2),
duplicate titles or descriptions, a missing mirror page, internal `.html` links, broken internal
links, invalid JSON-LD, and a `robots.txt` without the sitemap. `softwareVersion` is read from
`server.json`, so rerun it after a release.

## Social card

`public/assets/og.png` is `_tools/og.html` rendered at 1200×630:

```bash
cd site
python3 -m http.server 8811 --bind 127.0.0.1 &
chrome-use --session og open http://127.0.0.1:8811/_tools/og.html
chrome-use --session og set viewport 1200 630
chrome-use --session og screenshot public/assets/og.png
chrome-use --session og close
```

## Preview locally

```bash
cd site && npx wrangler@4 dev        # serves public/ the way production does (extensionless URLs, 404 page)
```

`python3 -m http.server -d site/public` also works for a quick look, but it does not map `/install`
to `install.html`.

## Deploy

From `site/`:

```bash
python3 _tools/seo.py --check && \
WA_PASSTHROUGH=1 CLOUDFLARE_API_TOKEN=... CLOUDFLARE_ACCOUNT_ID=3d050f54dac3fb90c344e3889ec45792 npx wrangler@4 deploy
```

The custom domain route creates the DNS record for `iphone-use.leeguoo.com`. After the first deploy:
submit `https://iphone-use.leeguoo.com/sitemap.xml` in Search Console (the `sc-domain:leeguoo.com`
property already covers the subdomain) and Bing Webmaster Tools, and check that GA4 is not counted
twice if a zone-wide analytics injector also runs on this hostname.
