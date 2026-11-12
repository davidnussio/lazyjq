# GIF of the jq function documentation while typing a query (README).
# Run from the repository root after `cargo build --locked`:
#   vhs assets/tapes/functions.tape
# then shrink it:
#   ffmpeg -i assets/tapes/functions.gif -filter_complex '[0:v]fps=12,split[a][b];[a]palettegen=max_colors=64:stats_mode=diff[p];[b][p]paletteuse=dither=none:diff_mode=rectangle' -loop 0 assets/tapes/functions.palette.gif
#   magick assets/tapes/functions.palette.gif -fuzz 3% -layers Optimize assets/screenshots/functions.gif
#   rm assets/tapes/functions.gif assets/tapes/functions.palette.gif
Output assets/tapes/functions.gif

Require cat

Set Shell "bash"
Set Theme "Dracula"
Set FontSize 18
Set Width 1400
Set Height 800
Set Padding 20
Set Framerate 12
Set CursorBlink false
Set TypingSpeed 90ms

Hide
Type `export PATH="$PWD/target/debug:$PATH"`
Enter
Type "unset NO_COLOR"
Enter
Type "clear"
Enter
Type "cat assets/tapes/demo.json | lazyjq -c assets/tapes/demo.toml"
Enter
Sleep 1500ms
Type@0ms ".spec.containers[0].command"
Enter
Sleep 500ms
Type "1"
Sleep 300ms
Show

Sleep 1s
# No path matches: the Suggestions panel lists jq functions,
# panel 3 shows the documentation of the selected one.
Type " | ma"
Sleep 2s
# Tab previews the candidates, Esc goes back to what was typed.
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Escape
Sleep 1s
Type "p("
Sleep 1500ms
# Typing the arguments: the documentation follows the innermost function.
Type "select("
Sleep 2s
Type 'startswith("--d'
Sleep 2s
Tab
Sleep 1500ms
Type ")))"
Sleep 1s
Enter
Sleep 3s
