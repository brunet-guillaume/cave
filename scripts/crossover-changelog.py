#!/usr/bin/env python3
"""Changes of a CrossOver version, as Markdown, from CodeWeavers' feed.

    curl -fsSL https://www.codeweavers.com/xml/versions/cxmac.xml |
        scripts/crossover-changelog.py <version>

The runtime workflow puts them in the notes of the wine-cx-<version> release.
(The feed comes from curl: CodeWeavers' server refuses Python's user agent.)
"""
import html
import re
import sys


def changelog(feed, version):
    """The <description> of the version's <item>: headings in bold, then
    one line per change."""
    for item in re.findall(r"<item>(.*?)</item>", feed, re.S):
        if f'sparkle:shortVersionString="{version}"' not in item:
            continue
        description = re.search(r"<description><!\[CDATA\[(.*?)\]\]></description>", item, re.S)
        if not description:
            return ""
        lines = []
        # Headings are <span class="subtitle">, changes the innermost <li>.
        pattern = r'<span class="subtitle">(.*?)</span>|<li>([^<]+)</li>'
        for heading, change in re.findall(pattern, description.group(1), re.S):
            if heading:
                lines.append(f"\n**{html.unescape(heading.strip())}**\n")
            elif change.strip():
                lines.append(f"- {html.unescape(' '.join(change.split()))}")
        return "\n".join(lines).strip()
    return ""


if __name__ == "__main__":
    print(changelog(sys.stdin.read(), sys.argv[1]) or "_No changes listed in CodeWeavers' feed._")
