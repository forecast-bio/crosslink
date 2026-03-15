"""
Forecast brand constants and SVG primitives for diagram generation.

Palette from docs_src/_brand.yml. Typography follows Forecast house style:
  - Helvetica for top-level headings (h1-equivalent)
  - Times New Roman italic for sub-headings (h2-equivalent)
  - Helvetica for body text
  - IBM Plex Mono ONLY for code references and slash-commands

Shape vocabulary: ellipses, rounded rectangles, circles, confetti dots.
Confetti dots use multiplicative blend mode with 50% opacity.
All arrows are solid (no dashed lines).
"""

import math
import random

# ── Palette (from _brand.yml) ────────────────────────────────────────────────

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

# Confetti dot colors — all solid brand colors, no ghost/translucent dots
CONFETTI_COLORS = [P["red"], P["green"], P["blue"], P["yellow"], P["pink"]]


# ── SVG boilerplate ──────────────────────────────────────────────────────────

def svg_header(width, height):
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}">
  <defs>
    <style>
      .heading {{ font-family: Helvetica, Arial, sans-serif; }}
      .subheading {{ font-family: 'Times New Roman', Times, serif; font-style: italic; }}
      .body {{ font-family: Helvetica, Arial, sans-serif; }}
      .mono {{ font-family: 'IBM Plex Mono', 'SF Mono', monospace; }}
    </style>
  </defs>
  <!-- no background rect — page bg shows through -->
"""


def svg_footer():
    return "</svg>\n"


# ── Shape primitives ─────────────────────────────────────────────────────────

def ellipse(cx, cy, rx, ry, fill, opacity=1.0, rotate=0):
    op = f' opacity="{opacity}"' if opacity < 1.0 else ""
    tr = f' transform="rotate({rotate} {cx} {cy})"' if rotate else ""
    return f'  <ellipse cx="{cx}" cy="{cy}" rx="{rx}" ry="{ry}" fill="{fill}"{op}{tr}/>\n'


def circle(cx, cy, r, fill, opacity=1.0):
    op = f' opacity="{opacity}"' if opacity < 1.0 else ""
    return f'  <circle cx="{cx}" cy="{cy}" r="{r}" fill="{fill}"{op}/>\n'


def rrect(x, y, w, h, fill, rx=None, opacity=1.0, rotate=0):
    if rx is None:
        rx = min(w, h) * 0.35
    op = f' opacity="{opacity}"' if opacity < 1.0 else ""
    tr = f' transform="rotate({rotate} {x + w/2} {y + h/2})"' if rotate else ""
    return f'  <rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" fill="{fill}"{op}{tr}/>\n'


# ── Text ──────────────────────────────────────────────────────────────────────

def text(x, y, content, cls="body", size=14, fill=None, anchor="middle", weight="normal"):
    f = fill or P["text"]
    w = f' font-weight="{weight}"' if weight != "normal" else ""
    return (f'  <text x="{x}" y="{y}" class="{cls}" font-size="{size}" '
            f'fill="{f}" text-anchor="{anchor}"{w}>{content}</text>\n')


# ── Arrows ────────────────────────────────────────────────────────────────────

def arrow_curved(x1, y1, x2, y2, color, stroke_width=2.5):
    dx, dy = x2 - x1, y2 - y1
    mx = (x1 + x2) / 2 + dy * 0.15
    my = (y1 + y2) / 2 - dx * 0.15
    angle = math.atan2(y2 - my, x2 - mx)
    hl = 10
    ax1 = x2 - hl * math.cos(angle - 0.35)
    ay1 = y2 - hl * math.sin(angle - 0.35)
    ax2 = x2 - hl * math.cos(angle + 0.35)
    ay2 = y2 - hl * math.sin(angle + 0.35)
    svg = (f'  <path d="M {x1:.1f} {y1:.1f} Q {mx:.1f} {my:.1f} {x2:.1f} {y2:.1f}" '
           f'fill="none" stroke="{color}" stroke-width="{stroke_width}" stroke-linecap="round"/>\n')
    svg += f'  <polygon points="{x2:.1f},{y2:.1f} {ax1:.1f},{ay1:.1f} {ax2:.1f},{ay2:.1f}" fill="{color}"/>\n'
    return svg


def arrow_straight(x1, y1, x2, y2, color, stroke_width=2.5):
    angle = math.atan2(y2 - y1, x2 - x1)
    hl = 10
    ax1 = x2 - hl * math.cos(angle - 0.35)
    ay1 = y2 - hl * math.sin(angle - 0.35)
    ax2 = x2 - hl * math.cos(angle + 0.35)
    ay2 = y2 - hl * math.sin(angle + 0.35)
    svg = (f'  <line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" '
           f'stroke="{color}" stroke-width="{stroke_width}" stroke-linecap="round"/>\n')
    svg += f'  <polygon points="{x2:.1f},{y2:.1f} {ax1:.1f},{ay1:.1f} {ax2:.1f},{ay2:.1f}" fill="{color}"/>\n'
    return svg


# ── Confetti dots (multiplicative blend, 50% opacity, bigger) ────────────────

CONFETTI_RADIUS = 8  # uniform size — tune this one value

def confetti(rng, x, y, w, h, count, colors=None, r=None):
    """Scattered confetti dots with multiply blend mode and 50% opacity."""
    if colors is None:
        colors = CONFETTI_COLORS
    dot_r = r if r is not None else CONFETTI_RADIUS
    svg = '  <g style="mix-blend-mode: multiply">\n'
    for _ in range(count):
        dx = x + rng.random() * w
        dy = y + rng.random() * h
        svg += f'    <circle cx="{dx:.1f}" cy="{dy:.1f}" r="{dot_r}" fill="{rng.choice(colors)}" opacity="0.5"/>\n'
    svg += '  </g>\n'
    return svg


# ── Composite helpers ─────────────────────────────────────────────────────────

def pill(x, y, w, h, color, label, rx=None, label_cls="mono"):
    """A small colored pill with a label inside."""
    if rx is None:
        rx = h / 2
    svg = rrect(x, y, w, h, color, rx=rx, opacity=0.18)
    svg += text(x + w / 2, y + h / 2 + 5, label, cls=label_cls, size=12, fill=color, weight="bold")
    return svg


def card(x, y, w, h, color, title, items):
    """A white card with a colored header band and bullet items.

    Items starting with a known command prefix are rendered in monospace.
    """
    svg = rrect(x, y, w, h, P["white"], rx=18, opacity=0.95)
    svg += rrect(x, y, w, 36, color, rx=18, opacity=0.15)
    svg += rrect(x, y + 18, w, 18, color, rx=0, opacity=0.15)
    svg += text(x + w / 2, y + 25, title, cls="subheading", size=15, fill=color, weight="bold")
    for j, item in enumerate(items):
        iy = y + 55 + j * 24
        svg += circle(x + 18, iy - 3, 4, color, opacity=0.5)
        # Detect command-like items and render in monospace
        is_cmd = any(item.startswith(p) for p in [
            "kickoff ", "swarm ", "crosslink ", "/", "knowledge ",
        ])
        cls = "mono" if is_cmd else "body"
        svg += text(x + 32, iy, item, cls=cls, size=13, fill=P["text"], anchor="start")
    return svg


def container(x, y, w, h, color, title, rx=30):
    """A bordered container with a subheading label."""
    svg = rrect(x, y, w, h, color, rx=rx, opacity=0.12)
    svg += rrect(x + 4, y + 4, w - 8, h - 8, P["white"], rx=rx - 2, opacity=0.85)
    svg += text(x + w / 2, y + 30, title, cls="subheading", size=18, fill=color)
    return svg


# ── CLI helper ────────────────────────────────────────────────────────────────

def write_svg(svg_content, args):
    """Write SVG to file or stdout based on argparse args."""
    if args.output:
        with open(args.output, "w") as f:
            f.write(svg_content)
        import sys
        print(f"Written: {args.output}", file=sys.stderr)
    else:
        print(svg_content)
