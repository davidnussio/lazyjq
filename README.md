# lazyjq

[![ci](https://github.com/davidnussio/lazyjq/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/davidnussio/lazyjq/actions/workflows/ci.yml)

*lazyjq* is a terminal UI to explore JSON with `jq`, laid out like
[lazygit](https://github.com/jesseduffield/lazygit): a query editor, side
panels with suggestions and history, and the result in the main panel, all on
screen at once and driven from the keyboard.

```text
╭[1] Query───────────────────────────────────────────────────────────────╮
│❯ .users[] | select(.city == "Lugano")                                  │
╰────────────────────────────────────────────────────────────────────────╯
╭[2] Suggestions · paths───╮╭[4] Result · 1 value───────────────────────╮
│.users                    ││{                                          │
│.users[0]                 ││  "name": "Carla",                         │
╰──────────────── 1 of 24 ─╯│  "age": 42,                               │
╭[3] History───────────────╮│  "tags": […], 1 item                      │
│.users | length           ││}                                          │
│.meta                     ││                                           │
╰───────────────── 1 of 2 ─╯╰─────────────────────────────── row 1 of 8 ╯
 fold: enter | drill down: f | copy: y | expand all: e | query: 1 | help: ?
```

lazyjq started as a fork of [jnv](https://github.com/ynqa/jnv) by ynqa and
evaluates filters with [jaq](https://github.com/01mf02/jaq), so `jq` doesn't
need to be installed.

## Features

- **Panels like lazygit**: Query, Suggestions, History and Result, with a
  focused panel, numbered shortcuts (`1`–`4`), a keybinding bar for the focused
  panel and a help popup (`?`).
- **Live suggestions**: the Suggestions panel follows what you type.
  - Paths of the input (`.users[0].name`) matching the query.
  - Values after a comparison or a string function, e.g.
    `.users[] | select(.name == "Al` or `select(.email | endswith("`.
  - jq functions when no path matches, e.g. `.users[] | sel` or after `| `:
    meanwhile panel 3 shows the documentation of the selected function
    (from the jq manual) in place of the History. While typing the arguments
    of a function (`select(.age > `), its documentation stays visible.
  - <kbd>Tab</kbd> completes: a single candidate is inserted directly, otherwise
    the candidates are previewed one by one (<kbd>Esc</kbd> restores the query).
- **Stable result**: while the query is incomplete or invalid, the Result panel
  keeps the last valid result and the error is shown on the Query panel border
  (e.g. `syntax error: missing closing quote`).
- **Result navigation**: fold/unfold nodes, fold/unfold everything,
  drill down into the selected node (the query becomes `… | .path.to.node`)
  and back up again, copy the path or the value of the selected node.
- **History** of the queries you ran, kept across sessions.
- Mouse support: click to focus/select, wheel to scroll.
- Fast on large inputs: the input is converted for jq once, evaluations run on
  a dedicated thread and are cancelled when superseded.
- Input from a file or stdin, one or more JSON values (e.g. [JSON Lines](https://jsonlines.org/)).
- Configurable layout, colors and keybindings (TOML).

## Installation

### Cargo

```bash
cargo install --git https://github.com/davidnussio/lazyjq
```

### From source

```bash
git clone https://github.com/davidnussio/lazyjq.git
cd lazyjq
cargo build --release
./target/release/lazyjq data.json
```

### Docker

```bash
docker build -t lazyjq .
docker run -it --rm -v $(pwd)/data.json:/data.json lazyjq /data.json
```

## Usage

```bash
cat data.json | lazyjq

# or
lazyjq data.json

# start with a filter
lazyjq --default-filter '.users[0]' data.json

# write the current result to stdout on exit (UNIX only)
cat data.json | lazyjq --write-to-stdout | some-command
cat data.json | lazyjq --write-to-stdout > result.json
```

```text
Usage: lazyjq [OPTIONS] [INPUT]

Arguments:
  [INPUT]  Optional path to a JSON file. If not provided or if "-" is specified, reads from standard input

Options:
  -c, --config <CONFIG_FILE>             Path to the configuration file.
      --default-filter <DEFAULT_FILTER>  Default jq filter to apply to the input data
      --write-to-stdout                  Write the current JSON result to stdout when exiting
  -h, --help                             Print help (see more with '--help')
  -V, --version                          Print version
```

## Keybindings

Press <kbd>?</kbd> (or <kbd>F1</kbd> while typing) to see all the keybindings.
Single-character bindings such as <kbd>q</kbd> or <kbd>?</kbd> don't apply while
typing in the query editor.

### Global

| Key | Action |
| :- | :- |
| <kbd>q</kbd>, <kbd>Ctrl + C</kbd> | Quit (<kbd>q</kbd> outside the query editor) |
| <kbd>?</kbd>, <kbd>F1</kbd> | Help |
| <kbd>1</kbd> <kbd>2</kbd> <kbd>3</kbd> <kbd>4</kbd> | Focus Query / Suggestions / History / Result |
| <kbd>/</kbd>, <kbd>i</kbd> | Focus the query editor |
| <kbd>←</kbd>, <kbd>→</kbd> | Previous / next panel |
| <kbd>Ctrl + Q</kbd> | Copy the query |
| <kbd>Ctrl + O</kbd> | Copy the result |

### [1] Query

| Key | Action |
| :- | :- |
| <kbd>Enter</kbd> | Run now, save to history and focus the result |
| <kbd>Esc</kbd> | Focus the result |
| <kbd>Tab</kbd> | Complete from Suggestions |
| <kbd>↑</kbd>, <kbd>↓</kbd> | Browse the history |
| <kbd>Ctrl + A</kbd>, <kbd>Ctrl + E</kbd> | Beginning / end of the line |
| <kbd>Alt + B</kbd>, <kbd>Alt + F</kbd> | Previous / next word |
| <kbd>Ctrl + W</kbd>, <kbd>Alt + D</kbd> | Delete the previous / next word |
| <kbd>Ctrl + U</kbd> | Clear the query |
| <kbd>Ctrl + K</kbd> | Delete to the end of the line |

### [2] Suggestions and [3] History

| Key | Action |
| :- | :- |
| <kbd>j</kbd>, <kbd>k</kbd>, <kbd>↓</kbd>, <kbd>↑</kbd>, <kbd>Tab</kbd> | Move |
| <kbd>g</kbd>, <kbd>G</kbd> | First / last item |
| <kbd>Enter</kbd> | Put the item in the query (History: and run it) |
| <kbd>Esc</kbd> | Back to the query (Suggestions: undo the completion) |
| <kbd>d</kbd> | Delete the history entry |

While panel 3 shows the documentation of a function, the move keys scroll it.

### [4] Result

| Key | Action |
| :- | :- |
| <kbd>j</kbd>, <kbd>k</kbd>, <kbd>↓</kbd>, <kbd>↑</kbd> | Move |
| <kbd>Ctrl + D</kbd>, <kbd>Ctrl + U</kbd> | Half page down / up |
| <kbd>g</kbd>, <kbd>G</kbd> | Top / bottom |
| <kbd>Enter</kbd>, <kbd>Space</kbd> | Fold / unfold |
| <kbd>l</kbd>, <kbd>h</kbd> | Unfold (or enter) / fold (or go to parent) |
| <kbd>e</kbd>, <kbd>c</kbd> | Unfold / fold everything |
| <kbd>f</kbd> | Narrow the query down to the selected node |
| <kbd>Backspace</kbd>, <kbd>u</kbd> | Back to the result before the last <kbd>f</kbd>, with cursor and folds as you left them |
| <kbd>y</kbd> | Copy menu: path or value of the selected node, result, query |
| <kbd>Esc</kbd> | Back to the query |

## Configuration

The configuration file is looked up in this order:

1. The path given with `-c` / `--config`
2. The default location, following the `dirs` crate:
   - **Linux**: `~/.config/lazyjq/config.toml`
   - **macOS**: `~/Library/Application Support/lazyjq/config.toml`
   - **Windows**: `C:\Users\{Username}\AppData\Roaming\lazyjq\config.toml`

If the file does not exist it is created with the defaults on the first run.
Your file is merged on top of the built-in defaults, so it only needs the
settings you want to change, for example:

```toml
[ui]
side_panel_width = 40
border = "Plain"

[theme]
selected_line = "bg=#44475A,attr=bold"

[keybinds.global]
quit = ["q", "Ctrl+D"]
```

See [default.toml](./default.toml) for every setting (layout, colors, JSON
styles, suggestions, history and keybindings) with its documentation. Styles
use the [termcfg notation](https://github.com/ynqa/termcfg/blob/main/Notations.md),
e.g. `"fg=blue,attr=bold"`.

The query history is stored in the data directory
(`~/.local/share/lazyjq/history` on Linux,
`~/Library/Application Support/lazyjq/history` on macOS);
set `history.persist = false` to keep it in memory only.

> [!NOTE]
> lazyjq doesn't read jnv's configuration: the layout and the settings are different.

## Credits

- [jnv](https://github.com/ynqa/jnv) by ynqa, which lazyjq is forked from
- [lazygit](https://github.com/jesseduffield/lazygit) for the interface
- [jaq](https://github.com/01mf02/jaq) for the `jq` implementation
- [ratatui](https://ratatui.rs) for the terminal UI
