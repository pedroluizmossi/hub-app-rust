# Themes, skins and backdrops from files

Examples for customizing MiniUtils without recompiling:

| Folder | What it is | Example |
|---|---|---|
| `themes/` | colors of an app theme (card and mascot) | [`themes/ocean.toml`](themes/ocean.toml) |
| `skins/` | window skin (frame, fonts, colors, backdrop) | [`skins/ocean.toml`](skins/ocean.toml) |
| `backdrops/` | panel backdrop, in layers (some animated) | [`backdrops/bubbles.toml`](backdrops/bubbles.toml) |
| `sprites/` | a 16 × 14 mascot that follows the app theme, with an animation | [`sprites/jellyfish.toml`](sprites/jellyfish.toml) |

To use them, copy the folders to the MiniUtils data folder (`miniutils --dirs` shows
where: `~/.local/share/miniutils/` on Linux, `%APPDATA%\miniutils\` on Windows;
`MINIUTILS_APPEARANCE` changes it) and click **Settings › Appearance › Reload**. The
"Ocean" skin shows up in **Window skin**, and the jellyfish in every mascot picker.

The format of each file, its fields and the layers are in
[DESIGN.md › Temas, visuais e fundos por arquivo](../../DESIGN.md#temas-visuais-e-fundos-por-arquivo).
