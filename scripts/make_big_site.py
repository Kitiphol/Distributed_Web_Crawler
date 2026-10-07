#!/usr/bin/env python3
"""Generate a larger, deterministic test site for multi-node and crash tests.

    python3 scripts/make_big_site.py [directory] [pages]
    python3 scripts/make_big_site.py test-big 200

Each page links to a few other pages (some through shared navigation, so many links
are discovered more than once), and every tenth page links to an image.
"""
import os, sys

out = sys.argv[1] if len(sys.argv) > 1 else "test-big"
pages = int(sys.argv[2]) if len(sys.argv) > 2 else 200
os.makedirs(os.path.join(out, "img"), exist_ok=True)
words = "alpha beta gamma delta epsilon zeta eta theta iota kappa".split()
for i in range(pages):
    links = sorted({(i * 7 + 1) % pages, (i * 13 + 5) % pages, (i + 1) % pages, 0})
    body = " ".join(words[(i + k) % len(words)] for k in range(5 + i % 7))
    html = [f"<!DOCTYPE html><html><head><title>Page {i}</title></head><body>",
            f"<nav><a href='p0.html'>Home</a> <a href='p1.html'>About</a></nav>",
            f"<p>{body}</p>"]
    html += [f"<a href='p{j}.html'>Page {j}</a>" for j in links if j != i]
    if i % 10 == 0:
        html.append(f"<img src='img/pic{i}.png'>")
        open(os.path.join(out, "img", f"pic{i}.png"), "wb").write(b"\x89PNG\r\n\x1a\n")
    html.append("</body></html>")
    open(os.path.join(out, f"p{i}.html"), "w").write("\n".join(html))
open(os.path.join(out, "index.html"), "w").write(
    "<!DOCTYPE html><html><body><p>Start here</p><a href='p0.html'>Begin</a></body></html>")
print(f"wrote {pages} pages to {out}/ (start at index.html)")
