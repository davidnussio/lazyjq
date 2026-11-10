# Run from the repository root after `cargo build --locked`:
#   vhs tapes/demo.tape
# Clean dark background noise, reduce the palette, then optimize similar frames:
#   ffmpeg -i tapes/demo.gif -filter_complex '[0:v]fps=12,format=rgba,colorkey=0x000000:0.2:0[fg];color=c=0x1e1f29:s=1200x720:r=12[bg];[bg][fg]overlay=shortest=1,split[a][b];[a]palettegen=max_colors=64:stats_mode=diff[p];[b][p]paletteuse=dither=none:diff_mode=rectangle' -loop 0 tapes/demo.palette.gif
#   magick tapes/demo.palette.gif -fuzz 3% -layers Optimize tapes/demo.gif
#   rm tapes/demo.palette.gif
Output tapes/demo.gif

Require cat
Require ./target/debug/lazyjq

Set Shell "bash"
Set Theme "Dracula"
Set FontSize 18
Set Width 1200
Set Height 720
Set Padding 20
Set Framerate 12
Set CursorBlink false
Set TypingSpeed 45ms

# Use this checkout and the demo config, independent of the user's settings.
Hide
Type `export PATH="$PWD/target/debug:$PATH"`
Enter
Type "unset NO_COLOR"
Enter
Type "clear"
Enter
Sleep 500ms
Show

Type "cat tapes/demo.json | lazyjq -c tapes/demo.toml"
Enter
Sleep 1500ms

# The Suggestions panel follows the query; Tab previews the candidates.
Type ".metadata.l"
Sleep 1s
Tab
Sleep 1s
Tab
Sleep 1s
Enter
Sleep 1s

# Value suggestions inside select().
Ctrl+U
Type@35ms '.spec.containers[] | select(.name == "'
Sleep 1500ms
Tab
Sleep 1s
Type ")"
Sleep 1s

# Run it (saved in History) and browse the result.
Enter
Sleep 1s
Down@300ms 4
Sleep 500ms
Space
Sleep 1s
Type "c"
Sleep 1s
Type "e"
Sleep 1s

# Drill down into the selected node.
Down@300ms 2
Type "f"
Sleep 1500ms

# Keybindings popup.
Type "?"
Sleep 2s
Escape
Sleep 500ms
Type "q"
Sleep 500ms
