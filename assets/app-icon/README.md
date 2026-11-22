# ShellCraft app icon

The **sea otter**: ShellCraft's icon, made by blamejs. An engraved sea otter floating on its back
and holding a scallop shell, drawn in ink `#080606` with paper `#fcf1de` highlights, on a rounded
tile in indigo `#271475`. The icon is not open source: see `LICENSE.txt`.

## Geometry

The tile is 512 units square with 127-unit corner radii (24.8 %), and the otter is clipped to it.
Everything outside the tile is transparent. The macOS icons place the tile on Apple's icon grid,
where it fills the central 824 of 1024 px and leaves a 100 px transparent margin on each side.
Every other render uses the full tile.

## Files

- `shellcraft.svg`: the master SVG and the hicolor scalable icon, about 480 KB.
- `shellcraft-1024.png`: a 1024 px render of the tile.
- `shellcraft.icns`: the macOS bundle icon (16 to 512 px at 1x and 2x).
- `shellcraft.ico`: the Windows icon (16, 20, 24, 32, 40, 48, 64, 128 and 256 px).
- `hicolor/<size>/apps/sh.shellcraft.ShellCraft.png` (16, 24, 32, 48, 64, 128, 256 and 512 px)
  and `hicolor/scalable/apps/sh.shellcraft.ShellCraft.svg`: the Linux icon theme.
- `LICENSE.txt`: the terms for the icon.

At 16 px the engraving reduces to the otter's light head and dark body on the indigo tile; from
32 px up, the face, paws and shell can be told apart.
