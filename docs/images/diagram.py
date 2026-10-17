#!/usr/bin/python3 -I
"""Draws docs/images/how-it-works-{light,dark}.svg, the README's diagram: two witnesses feed
osreflogd, which keeps the ledger and serves reading, undo and bisect.

    python3 -I docs/images/diagram.py
"""

import os

W, H = 960, 440
THEMES = {
    "light": dict(text="#1f2328", dim="#59636e", card="#f6f8fa", line="#d1d9e0",
                  accent="#c4581a", accent_bg="#fdf1e9", blue="#2f6fb5", blue_bg="#eaf2fb",
                  arrow="#8c959f"),
    "dark": dict(text="#e6edf3", dim="#9198a1", card="#151b23", line="#3d444d",
                 accent="#f0883e", accent_bg="#2b1d12", blue="#6ca4e6", blue_bg="#132235",
                 arrow="#6e7681"),
}
FONT = "font-family=\"-apple-system,'Segoe UI',Cantarell,'Noto Sans',sans-serif\""


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def card(x, y, w, h, title, lines, t, tone=None):
    fill, stroke, head = t["card"], t["line"], t["text"]
    if tone == "accent":
        fill, stroke, head = t["accent_bg"], t["accent"], t["accent"]
    elif tone == "blue":
        fill, stroke, head = t["blue_bg"], t["blue"], t["blue"]
    out = [f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="12" fill="{fill}" stroke="{stroke}" stroke-width="1.5"/>',
           f'<text x="{x + 16}" y="{y + 28}" {FONT} font-size="16" font-weight="700" fill="{head}">{esc(title)}</text>']
    for i, line in enumerate(lines):
        out.append(f'<text x="{x + 16}" y="{y + 52 + i * 20}" {FONT} font-size="13" fill="{t["dim"]}">{esc(line)}</text>')
    return out


def arrow(x1, y1, x2, y2, t, label=None):
    out = [f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{t["arrow"]}" stroke-width="1.75" marker-end="url(#head)"/>']
    if label:
        out.append(f'<text x="{(x1 + x2) / 2}" y="{(y1 + y2) / 2 - 8}" {FONT} font-size="12" fill="{t["dim"]}" text-anchor="middle">{esc(label)}</text>')
    return out


def draw(t):
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" '
             'aria-label="Two witnesses feed osreflogd: the live witness records who changed each file, the state '
             'witness proves what changed between btrfs snapshots. osreflogd groups changes into causes in a '
             'hash-chained ledger, and serves reading (CLI and app), undo and bisect.">',
             f'<defs><marker id="head" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto">'
             f'<path d="M0,0 L10,5 L0,10 z" fill="{t["arrow"]}"/></marker></defs>']
    # The two witnesses.
    parts += card(20, 40, 250, 150, "Live witness: who", [
        "fanotify sees every write", "eBPF traces the process to its", "command, unit or dnf transaction",
        "D-Bus: the caller who asked"], t, "blue")
    parts += card(20, 240, 250, 150, "State witness: what", [
        "read-only btrfs snapshots", "around every dnf transaction,", "at boot, daily, on request;",
        "diffed from the send stream"], t, "blue")
    # The daemon.
    parts += card(340, 40, 280, 350, "osreflogd", [], t, "accent")
    parts += card(360, 90, 240, 116, "Causes", [
        "changes grouped by what", "made them: a package, a unit,", "a command, an app"], t)
    parts += card(360, 228, 240, 140, "Ledger", [
        "SQLite, hash-chained", "content of changed configs", "(never secrets), compressed,",
        "the attic undo keeps"], t)
    parts += arrow(480, 206, 480, 226, t)
    # Inputs into the daemon.
    parts += arrow(270, 115, 356, 140, t)
    parts += arrow(270, 315, 356, 290, t)
    parts += [f'<text x="145" y="212" {FONT} font-size="12" fill="{t["dim"]}" text-anchor="middle">the two must agree;</text>',
              f'<text x="145" y="228" {FONT} font-size="12" fill="{t["dim"]}" text-anchor="middle">a gap between them is reported</text>']
    # What it serves.
    parts += card(690, 40, 250, 104, "Read", ["log · diff · why · show", "CLI and GNOME app, D-Bus + polkit"], t)
    parts += card(690, 163, 250, 104, "Undo", ["a dry run first, a checkpoint,", "conflicts left alone"], t)
    parts += card(690, 286, 250, 104, "Bisect", ["down to one cause, in copies:", "containers · VMs · real boots"], t)
    for y in (92, 215, 338):
        parts += arrow(620, 215 if y == 215 else (150 if y == 92 else 280), 686, y, t)
    parts.append(f'<text x="{W / 2}" y="{H - 14}" {FONT} font-size="12" fill="{t["dim"]}" text-anchor="middle">'
                 'Root, sandboxed by systemd; the CLI and the app only ever ask it over D-Bus.</text>')
    parts.append("</svg>")
    return "\n".join(parts) + "\n"


if __name__ == "__main__":
    here = os.path.dirname(os.path.abspath(__file__))
    for name, theme in THEMES.items():
        path = os.path.join(here, f"how-it-works-{name}.svg")
        with open(path, "w") as f:
            f.write(draw(theme))
        print(path)
