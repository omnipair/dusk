#!/usr/bin/env python3
"""Add deterministic print-only layout rules to the portable report.

The Data Analytics portable reader intentionally keeps wide tables horizontally
scrollable on screen. Chromium prints those max-content tables at their screen
width, which clips columns at the page boundary. This post-packaging step keeps
the validated interactive layout unchanged and constrains only print media.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path


STYLE_ID = "dusk-print-layout-v1"
PRINT_STYLE = f"""<style id=\"{STYLE_ID}\">
@media print {{
  @page {{ size: Letter portrait; margin: 0.45in; }}

  html,
  body,
  .portable-fallback,
  .portable-block-stack,
  .portable-block,
  .portable-content-card,
  .portable-table-source-region,
  .portable-table-scroll {{
    min-width: 0 !important;
    max-width: 100% !important;
  }}

  .portable-table-card {{
    break-inside: auto !important;
  }}

  .portable-visual-header,
  .portable-content-card > .portable-inline-source {{
    break-after: avoid !important;
  }}

  .portable-inline-source,
  .portable-inline-source-content,
  .portable-source-summary-content {{
    break-inside: avoid !important;
  }}

  .portable-content-card > .portable-table-source-region,
  .portable-content-card > .portable-static-chart {{
    break-before: avoid !important;
  }}

  .portable-markdown p:has(+ ul),
  .portable-markdown p:has(+ ol) {{
    break-after: avoid !important;
  }}

  .portable-markdown h1,
  .portable-markdown h2,
  .portable-markdown h3 {{
    break-after: avoid !important;
  }}

  .portable-markdown > :last-child {{
    break-after: avoid !important;
  }}

  .portable-table-source-region,
  .portable-table-scroll {{
    width: 100% !important;
    overflow: visible !important;
  }}

  .portable-table-scroll table {{
    width: 100% !important;
    min-width: 0 !important;
    max-width: 100% !important;
    table-layout: fixed !important;
  }}

  .portable-table-scroll th,
  .portable-table-scroll td {{
    max-width: 100% !important;
    padding: 3px 4px !important;
    overflow: visible !important;
    color: var(--portable-table-text) !important;
    font-size: 7.25pt !important;
    line-height: 1.22 !important;
    text-overflow: clip !important;
    overflow-wrap: anywhere !important;
    word-break: normal !important;
    white-space: normal !important;
  }}

  .portable-table-scroll th {{
    color: var(--portable-tertiary) !important;
  }}

  .portable-table-scroll td:first-child {{
    color: var(--portable-ink) !important;
  }}

  [data-table-id="native-route-table"] th,
  [data-table-id="native-route-table"] td,
  [data-table-id="native-lp-leader-table"] th,
  [data-table-id="native-lp-leader-table"] td,
  [data-table-id="native-cpmm-table"] th,
  [data-table-id="native-cpmm-table"] td {{
    font-size: 6.25pt !important;
    padding-inline: 2px !important;
  }}

  [data-table-id="native-predicate-table"] th:nth-child(1),
  [data-table-id="native-predicate-table"] td:nth-child(1) {{ width: 25%; }}
  [data-table-id="native-predicate-table"] th:nth-child(2),
  [data-table-id="native-predicate-table"] td:nth-child(2) {{ width: 25%; }}
  [data-table-id="native-predicate-table"] th:nth-child(3),
  [data-table-id="native-predicate-table"] td:nth-child(3) {{ width: 42%; }}
  [data-table-id="native-predicate-table"] th:nth-child(4),
  [data-table-id="native-predicate-table"] td:nth-child(4) {{ width: 8%; }}

  .portable-static-chart,
  .portable-static-chart-variant,
  .portable-static-chart-variant > svg {{
    min-width: 0 !important;
    max-width: 100% !important;
  }}
}}
</style>"""


def inject_print_style(html: str) -> str:
    """Return ``html`` with exactly one print-style block installed."""

    opening = f'<style id="{STYLE_ID}">'
    occurrences = html.count(opening)
    if occurrences > 1:
        raise ValueError(f"portable report contains {occurrences} {STYLE_ID} blocks")
    if occurrences == 1:
        start = html.index(opening)
        end = html.find("</style>", start)
        if end < 0:
            raise ValueError(f"portable report has an unterminated {STYLE_ID} block")
        end += len("</style>")
        return html[:start] + PRINT_STYLE + html[end:]
    closing_head = html.lower().find("</head>")
    if closing_head < 0:
        raise ValueError("portable report is missing </head>")
    return html[:closing_head] + PRINT_STYLE + "\n" + html[closing_head:]


def finalize(path: Path) -> bool:
    """Finalize ``path`` atomically; return whether the file changed."""

    original = path.read_text(encoding="utf-8")
    finalized = inject_print_style(original)
    if finalized == original:
        return False

    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    temporary.write_text(finalized, encoding="utf-8")
    os.replace(temporary, path)
    return True


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Install print-safe table layout in packaged report.html"
    )
    parser.add_argument(
        "path",
        nargs="?",
        type=Path,
        default=Path(__file__).resolve().parent / "report.html",
    )
    args = parser.parse_args()
    changed = finalize(args.path.resolve())
    action = "updated" if changed else "already finalized"
    print(f"{action}: {args.path.resolve()}")


if __name__ == "__main__":
    main()
