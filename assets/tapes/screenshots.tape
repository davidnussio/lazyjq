# Screenshots used in the README (assets/screenshots/*.png).
# Run from the repository root after `cargo build --locked`:
#   vhs assets/tapes/screenshots.tape
# then shrink them:
#   for f in assets/screenshots/*.png; do magick "$f" -dither None -colors 128 -strip "PNG8:$f"; done
#   rm assets/tapes/screenshots.gif
Output assets/tapes/screenshots.gif

Require cat

Set Shell "bash"
Set Theme "Dracula"
Set FontSize 18
Set Width 1400
Set Height 800
Set Padding 20
Set CursorBlink false
Set TypingSpeed 0ms

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
Show

# A query run (saved in History) and its result.
Type ".metadata.labels"
Enter
Sleep 500ms
Type "1"
Ctrl+U
Type ".spec.containers[] | {name, image, mounts: [.volumeMounts[].mountPath]}"
Enter
Sleep 1s
Screenshot assets/screenshots/result.png
Sleep 500ms

# Paths of the input matching the query.
Type "1"
Ctrl+U
Type ".spec.containers[0].livenessProbe"
Enter
Sleep 500ms
Type "1"
Type "."
Sleep 1s
Screenshot assets/screenshots/paths.png
Sleep 500ms

# Values after a comparison.
Ctrl+U
Type ".spec.volumes"
Enter
Sleep 500ms
Type "1"
Type '[] | select(.name == "'
Sleep 1500ms
Screenshot assets/screenshots/values.png
Sleep 500ms

# jq functions, with the documentation of the selected one in panel 3.
Ctrl+U
Type ".spec.containers[0].command"
Enter
Sleep 500ms
Type "1"
Type " | ma"
Sleep 1s
Screenshot assets/screenshots/functions.png
Sleep 500ms

# The documentation follows the function whose arguments are typed.
Type 'p(select(startswith("--data'
Sleep 1500ms
Screenshot assets/screenshots/function-arguments.png
Sleep 500ms

Type "q"
