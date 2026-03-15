#!/usr/bin/env python3
"""
Generate a Forecast-styled diagram for knowledge management.

Shows: Agents write knowledge → synced via git → searchable by all agents

Usage:
    python3 scripts/generate-diagram-knowledge.py -o docs_src/assets/img/knowledge-flow.svg
"""

import argparse
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from brand import (P, svg_header, svg_footer, ellipse, circle, rrect, text,
                   arrow_curved, arrow_straight, confetti, pill, container,
                   write_svg)

WIDTH = 680
HEIGHT = 480
SEED = 37


def generate():
    rng = random.Random(SEED)
    svg = svg_header(WIDTH, HEIGHT)

    cx = WIDTH / 2

    # ── Title ─────────────────────────────────────────────────────────────
    svg += text(cx, 36, "Knowledge management", cls="heading", size=22, fill=P["black"])
    svg += text(cx, 56, "shared research pages synced via git",
                cls="subheading", size=14, fill=P["muted"])

    # ── Writers (left column) ─────────────────────────────────────────────
    svg += text(115, 95, "Writers", cls="heading", size=16, fill=P["black"])

    writers = [
        (115, 140, "Agent A", P["blue"],   "researches API patterns"),
        (115, 210, "Agent B", P["green"],  "documents architecture"),
        (115, 280, "Human",   P["pink"],   "imports design docs"),
    ]
    for wx, wy, name, color, desc in writers:
        svg += ellipse(wx, wy, 65, 30, color, opacity=0.15)
        svg += ellipse(wx, wy, 55, 22, P["white"], opacity=0.85)
        svg += text(wx, wy - 2, name, cls="body", size=13, fill=color, weight="bold")
        svg += text(wx, wy + 18, desc, cls="body", size=10, fill=P["muted"])

    # ── Arrows: writers → knowledge branch ────────────────────────────────
    for wy in [140, 210, 280]:
        svg += arrow_straight(180, wy, 240, 210, P["muted"], stroke_width=1.5)

    # ── Knowledge branch (center) ─────────────────────────────────────────
    kb_x, kb_y = 250, 130
    kb_w, kb_h = 200, 175
    svg += rrect(kb_x, kb_y, kb_w, kb_h, P["yellow"], rx=30, opacity=0.12)
    svg += rrect(kb_x + 4, kb_y + 4, kb_w - 8, kb_h - 8, P["white"], rx=28, opacity=0.85)
    svg += text(kb_x + kb_w / 2, kb_y + 30, "crosslink/knowledge",
                cls="subheading", size=18, fill=P["black"])

    # Knowledge pages inside
    pages = [
        ("api-patterns",    P["blue"]),
        ("auth-design",     P["green"]),
        ("error-handling",  P["red"]),
        ("deploy-runbook",  P["yellow"]),
    ]
    for i, (slug, color) in enumerate(pages):
        py = kb_y + 48 + i * 28
        svg += rrect(kb_x + 15, py, kb_w - 30, 22, color, rx=11, opacity=0.12)
        svg += text(kb_x + kb_w / 2, py + 15, slug, cls="mono", size=11, fill=color)

    # ── Arrows: knowledge → readers ───────────────────────────────────────
    for wy in [140, 210, 280]:
        svg += arrow_straight(455, 210, 505, wy, P["muted"], stroke_width=1.5)

    # ── Readers (right column) ────────────────────────────────────────────
    svg += text(575, 95, "Readers", cls="heading", size=16, fill=P["black"])

    readers = [
        (575, 140, "Any agent", P["blue"],   "knowledge search"),
        (575, 210, "Kickoff",   P["green"],  "auto-injected context"),
        (575, 280, "Swarm",     P["red"],    "shared across phases"),
    ]
    for rx, ry, name, color, desc in readers:
        svg += ellipse(rx, ry, 65, 30, color, opacity=0.15)
        svg += ellipse(rx, ry, 55, 22, P["white"], opacity=0.85)
        svg += text(rx, ry - 2, name, cls="body", size=13, fill=color, weight="bold")
        svg += text(rx, ry + 18, desc, cls="body", size=10, fill=P["muted"])

    # ── Sync indicator ────────────────────────────────────────────────────
    svg += text(cx, 335, "synced via git push/pull", cls="body", size=12, fill=P["muted"])

    # ── Bottom: capabilities row ──────────────────────────────────────────
    svg += rrect(50, 365, WIDTH - 100, 85, P["gray"], rx=20)

    caps = [
        (130,  P["blue"],   "Full-text search",  "knowledge search &lt;query&gt;"),
        (340,  P["green"],  "Bulk import",        "knowledge import &lt;dir&gt;"),
        (540,  P["red"],    "Tagged &amp; filtered",  "--tag, --since, --contributor"),
    ]
    for capx, color, title, desc in caps:
        svg += text(capx, 395, title, cls="heading", size=14, fill=color)
        svg += text(capx, 416, desc, cls="mono", size=10, fill=P["muted"])
        svg += text(capx, 434, "", cls="body", size=10, fill=P["muted"])

    # ── Confetti ──────────────────────────────────────────────────────────
    svg += confetti(rng, 10, 80, 40, 60, 4)
    svg += confetti(rng, 630, 80, 40, 60, 4)

    svg += svg_footer()
    return svg


def main():
    parser = argparse.ArgumentParser(description="Generate knowledge management diagram SVG")
    parser.add_argument("-o", "--output", help="Output file (default: stdout)")
    args = parser.parse_args()
    write_svg(generate(), args)


if __name__ == "__main__":
    main()
