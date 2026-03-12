#!/usr/bin/env python3
"""
Generate a Forecast-styled architectural diagram for the crosslink kickoff flow.

Shows: Human → /kickoff → [branch, worktree, agent identity] → Agent works → Results

Uses Forecast brand shapes (ellipses, rounded rects, crescents, dots) as
containers for diagram elements, with Helvetica body text and Times italic labels.

Usage:
    python3 scripts/generate-diagram-kickoff.py -o docs_src/assets/img/kickoff-flow.svg
"""

import argparse
import math
import random
import sys

# ── Forecast Brand Palette ──────────────────────────────────────────────────
P = {
    "red":    "#F95838",
    "green":  "#007C35",
    "blue":   "#00A6DB",
    "yellow": "#FFCE02",
    "pink":   "#FFB6C6",
    "bg":     "#F9F4F5",
    "white":  "#FFFFFF",
    "black":  "#000000",
    "gray":   "rgba(0,0,0,0.06)",
    "text":   "#1a1a1a",
    "muted":  "#666666",
}

WIDTH = 1100
HEIGHT = 620
SEED = 7


def svg_header():
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {HEIGHT}" width="{WIDTH}" height="{HEIGHT}">
  <defs>
    <style>
      .label {{ font-family: 'Times New Roman', Times, serif; font-style: italic; }}
      .body {{ font-family: Helvetica, Arial, sans-serif; }}
      .mono {{ font-family: 'IBM Plex Mono', 'SF Mono', monospace; }}
    </style>
  </defs>
  <rect width="{WIDTH}" height="{HEIGHT}" fill="{P['bg']}"/>
"""


def svg_footer():
    return "</svg>\n"


# ── Shape primitives ────────────────────────────────────────────────────────

def ellipse(cx, cy, rx, ry, fill, opacity=1.0):
    op = f' opacity="{opacity}"' if opacity < 1.0 else ""
    return f'  <ellipse cx="{cx}" cy="{cy}" rx="{rx}" ry="{ry}" fill="{fill}"{op}/>\n'


def circle(cx, cy, r, fill, opacity=1.0):
    op = f' opacity="{opacity}"' if opacity < 1.0 else ""
    return f'  <circle cx="{cx}" cy="{cy}" r="{r}" fill="{fill}"{op}/>\n'


def rrect(x, y, w, h, fill, rx=None, opacity=1.0):
    if rx is None:
        rx = min(w, h) * 0.35
    op = f' opacity="{opacity}"' if opacity < 1.0 else ""
    return f'  <rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" fill="{fill}"{op}/>\n'


def text(x, y, content, cls="body", size=14, fill=None, anchor="middle", weight="normal"):
    f = fill or P["text"]
    w = f' font-weight="{weight}"' if weight != "normal" else ""
    return (f'  <text x="{x}" y="{y}" class="{cls}" font-size="{size}" '
            f'fill="{f}" text-anchor="{anchor}"{w}>{content}</text>\n')


def arrow_path(x1, y1, x2, y2, color, stroke_width=2.5, dashed=False):
    """Curved arrow between two points."""
    # Determine curve direction based on relative positions
    dx = x2 - x1
    dy = y2 - y1
    dist = math.sqrt(dx * dx + dy * dy)

    # Control point offset for a gentle curve
    cx_off = dy * 0.15
    cy_off = -dx * 0.15
    mx = (x1 + x2) / 2 + cx_off
    my = (y1 + y2) / 2 + cy_off

    # Arrowhead
    angle = math.atan2(y2 - my, x2 - mx)
    head_len = 10
    ax1 = x2 - head_len * math.cos(angle - 0.35)
    ay1 = y2 - head_len * math.sin(angle - 0.35)
    ax2 = x2 - head_len * math.cos(angle + 0.35)
    ay2 = y2 - head_len * math.sin(angle + 0.35)

    dash = ' stroke-dasharray="6 4"' if dashed else ""
    svg = (f'  <path d="M {x1:.1f} {y1:.1f} Q {mx:.1f} {my:.1f} {x2:.1f} {y2:.1f}" '
           f'fill="none" stroke="{color}" stroke-width="{stroke_width}"{dash} '
           f'stroke-linecap="round"/>\n')
    svg += (f'  <polygon points="{x2:.1f},{y2:.1f} {ax1:.1f},{ay1:.1f} {ax2:.1f},{ay2:.1f}" '
            f'fill="{color}"/>\n')
    return svg


def straight_arrow(x1, y1, x2, y2, color, stroke_width=2.5, dashed=False):
    """Straight arrow between two points."""
    angle = math.atan2(y2 - y1, x2 - x1)
    head_len = 10
    ax1 = x2 - head_len * math.cos(angle - 0.35)
    ay1 = y2 - head_len * math.sin(angle - 0.35)
    ax2 = x2 - head_len * math.cos(angle + 0.35)
    ay2 = y2 - head_len * math.sin(angle + 0.35)

    dash = ' stroke-dasharray="6 4"' if dashed else ""
    svg = (f'  <line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" '
           f'stroke="{color}" stroke-width="{stroke_width}"{dash} stroke-linecap="round"/>\n')
    svg += (f'  <polygon points="{x2:.1f},{y2:.1f} {ax1:.1f},{ay1:.1f} {ax2:.1f},{ay2:.1f}" '
            f'fill="{color}"/>\n')
    return svg


def dots_in_region(rng, x, y, w, h, count, colors, r_min=1.5, r_max=4):
    svg = ""
    for _ in range(count):
        dx = x + rng.random() * w
        dy = y + rng.random() * h
        r = r_min + rng.random() * (r_max - r_min)
        svg += circle(dx, dy, r, rng.choice(colors), opacity=0.5 + rng.random() * 0.5)
    return svg


# ── Diagram composition ─────────────────────────────────────────────────────

def generate():
    rng = random.Random(SEED)
    svg = svg_header()

    # ── Title ───────────────────────────────────────────────────────────
    svg += text(WIDTH / 2, 40, "Kickoff Agent Lifecycle", cls="label", size=26, fill=P["black"])
    svg += text(WIDTH / 2, 62, "from instruction to autonomous implementation", cls="body", size=13, fill=P["muted"])

    # ── Phase 1: Human (left) ───────────────────────────────────────────
    # Pink ellipse container
    hx, hy = 130, 175
    svg += ellipse(hx, hy, 105, 80, P["pink"], opacity=0.5)
    svg += ellipse(hx, hy, 90, 65, P["pink"], opacity=0.35)
    svg += text(hx, hy - 18, "Human", cls="label", size=20, fill=P["black"])
    svg += text(hx, hy + 5, '"fix the auth bug"', cls="mono", size=11, fill=P["muted"])
    svg += text(hx, hy + 25, "high-level instruction", cls="body", size=11, fill=P["muted"])

    # ── Arrow: Human → Kickoff ──────────────────────────────────────────
    svg += arrow_path(230, 175, 315, 175, P["red"], stroke_width=2.5)
    svg += text(273, 163, "/kickoff", cls="mono", size=11, fill=P["red"], weight="bold")

    # ── Phase 2: Crosslink Orchestration (center) ───────────────────────
    # Green rounded rect as the main orchestration container
    ox, oy, ow, oh = 325, 100, 260, 155
    svg += rrect(ox, oy, ow, oh, P["green"], rx=30, opacity=0.12)
    svg += rrect(ox + 4, oy + 4, ow - 8, oh - 8, P["white"], rx=28, opacity=0.85)
    svg += text(ox + ow / 2, oy + 28, "Crosslink Orchestrates", cls="label", size=17, fill=P["green"])

    # Three sub-steps as small colored pills
    steps = [
        (ox + 30,  oy + 48, 110, 28, P["blue"],   "git branch"),
        (ox + 30,  oy + 82, 110, 28, P["yellow"], "git worktree"),
        (ox + 148, oy + 48, 110, 28, P["red"],    "agent init"),
        (ox + 148, oy + 82, 110, 28, P["green"],  "issue + session"),
    ]
    for sx, sy, sw, sh, color, label in steps:
        svg += rrect(sx, sy, sw, sh, color, rx=14, opacity=0.18)
        svg += text(sx + sw / 2, sy + sh / 2 + 5, label, cls="mono", size=10.5, fill=color, weight="bold")

    # Small accent dots around the orchestration box
    svg += dots_in_region(rng, ox - 15, oy - 15, ow + 30, oh + 30, 12,
                          [P["green"], P["blue"], P["yellow"], P["gray"]], r_min=1.5, r_max=3.5)

    # ── Arrow: Orchestration → Agent ────────────────────────────────────
    svg += arrow_path(590, 175, 670, 175, P["green"], stroke_width=2.5)
    svg += text(630, 163, "launch", cls="body", size=11, fill=P["green"])

    # ── Phase 3: Autonomous Agent (right) ───────────────────────────────
    # Blue ellipse container — the agent working zone
    ax, ay = 830, 175
    svg += ellipse(ax, ay, 160, 95, P["blue"], opacity=0.1)
    svg += ellipse(ax, ay, 145, 80, P["white"], opacity=0.8)
    svg += text(ax, ay - 40, "Autonomous Agent", cls="label", size=17, fill=P["blue"])

    # Agent activity list
    activities = [
        "explore codebase",
        "implement feature",
        "run tests + lint",
        "commit with /commit",
        "self-review",
    ]
    for i, act in enumerate(activities):
        yy = ay - 18 + i * 18
        svg += circle(ax - 80, yy - 4, 3, P["blue"], opacity=0.6)
        svg += text(ax - 70, yy, act, cls="mono", size=10.5, fill=P["text"], anchor="start")

    # ── Agent loop arrow (cycles back on itself) ────────────────────────
    svg += f'  <path d="M {ax + 100} {ay - 40} A 50 70 0 1 1 {ax + 100} {ay + 50}" '\
           f'fill="none" stroke="{P["blue"]}" stroke-width="1.5" stroke-dasharray="4 3" opacity="0.4"/>\n'
    svg += text(ax + 138, ay + 10, "iterate", cls="body", size=10, fill=P["blue"])

    # ── Phase 4: Results (bottom) ───────────────────────────────────────
    # Three result nodes connected back

    # Result container — wide rounded rect
    ry_base = 370
    svg += rrect(130, ry_base, 840, 210, P["gray"], rx=30, opacity=1.0)

    svg += text(WIDTH / 2, ry_base + 30, "Outputs", cls="label", size=20, fill=P["black"])

    # Result cards as colored rounded rects
    cards = [
        (160,  ry_base + 50, 175, 135, P["green"],  "Feature Branch",
         ["committed code", "tests passing", "clean clippy"]),
        (365,  ry_base + 50, 175, 135, P["yellow"], "Crosslink Trail",
         ["issue comments", "breadcrumbs", "handoff notes"]),
        (570,  ry_base + 50, 175, 135, P["blue"],   "Kickoff Report",
         ["spec validation", "phase timings", "verdict: pass/fail"]),
        (775,  ry_base + 50, 175, 135, P["red"],    "Ready for Review",
         ["draft PR (if --verify ci)", "self-review done", "status: DONE"]),
    ]

    for cx_, cy_, cw, ch, color, title, items in cards:
        svg += rrect(cx_, cy_, cw, ch, P["white"], rx=18, opacity=0.95)
        svg += rrect(cx_, cy_, cw, 36, color, rx=18, opacity=0.15)
        # Clip the bottom corners of the header
        svg += rrect(cx_, cy_ + 18, cw, 18, color, rx=0, opacity=0.15)
        svg += text(cx_ + cw / 2, cy_ + 24, title, cls="label", size=13, fill=color, weight="bold")

        for j, item in enumerate(items):
            iy = cy_ + 52 + j * 22
            svg += circle(cx_ + 18, iy - 3, 3.5, color, opacity=0.5)
            svg += text(cx_ + 30, iy, item, cls="body", size=11, fill=P["text"], anchor="start")

    # ── Arrows: Agent → Results ─────────────────────────────────────────
    svg += straight_arrow(830, 270, 830, ry_base + 45, P["blue"], stroke_width=2, dashed=True)
    svg += straight_arrow(455, 258, 455, ry_base + 45, P["green"], stroke_width=2, dashed=True)

    # ── Decorative elements ─────────────────────────────────────────────

    # Scattered dots around the diagram edges
    svg += dots_in_region(rng, 20, 280, 100, 80, 8,
                          [P["pink"], P["red"], P["gray"]], r_min=1.5, r_max=3)
    svg += dots_in_region(rng, 980, 90, 100, 100, 10,
                          [P["blue"], P["green"], P["gray"]], r_min=1.5, r_max=3)
    svg += dots_in_region(rng, 50, 520, 60, 60, 6,
                          [P["yellow"], P["pink"], P["gray"]], r_min=1.5, r_max=3)
    svg += dots_in_region(rng, 990, 450, 80, 80, 8,
                          [P["red"], P["green"], P["gray"]], r_min=1.5, r_max=3)

    # Crescent accent — top right
    svg += f'  <g opacity="0.25">\n'
    svg += f'    <mask id="deco-crescent">\n'
    svg += f'      <rect x="1000" y="20" width="100" height="100" fill="white"/>\n'
    svg += f'      <circle cx="1060" cy="62" r="28" fill="black"/>\n'
    svg += f'    </mask>\n'
    svg += f'    <circle cx="1050" cy="58" r="35" fill="{P["pink"]}" mask="url(#deco-crescent)"/>\n'
    svg += f'  </g>\n'

    # Small triangle accent — bottom left
    h_tri = 20 * math.sqrt(3) / 2
    svg += (f'  <polygon points="80,{HEIGHT - 30} {60},{HEIGHT - 30 + h_tri:.1f} '
            f'{100},{HEIGHT - 30 + h_tri:.1f}" fill="{P["green"]}" opacity="0.3"/>\n')

    svg += svg_footer()
    return svg


def main():
    parser = argparse.ArgumentParser(description="Generate kickoff flow diagram SVG")
    parser.add_argument("-o", "--output", help="Output file (default: stdout)")
    args = parser.parse_args()

    svg_content = generate()

    if args.output:
        with open(args.output, "w") as f:
            f.write(svg_content)
        print(f"Written: {args.output}", file=sys.stderr)
    else:
        print(svg_content)


if __name__ == "__main__":
    main()
