# scopic

A side-by-side terminal viewer for git and jj diffs. Pipe a diff in, read it
like the GitHub split view, press `q`.

```
jj diff --git | scopic
git diff | scopic
scopic changes.diff
```

Intraline word highlights, dual line numbers, file-to-file jumping. Colored
input is fine — ANSI codes are stripped before parsing. Anything scopic
doesn't understand renders as dim text instead of erroring. Empty input exits
silently, like a well-behaved pager.

## Keys

`j/k` scroll · `ctrl-d/u` half-page · `n/p` next/prev file · `g/G` top/bottom · `q` quit

## Install

```
cargo install --path .
```

## Use as the default diff pager

**jj** — add to `~/.config/jj/config.toml` (scoped so `jj log` etc. keep the
normal pager):

```toml
[[--scope]]
--when.commands = ["diff", "show"]
ui.diff-formatter = ":git"
ui.pager = ["scopic"]
```

**git**:

```
git config --global pager.diff scopic
git config --global pager.show scopic
```

Then `jj diff`, `jj show`, and `git diff` open scopic automatically.

## Notes

~500 lines, three source files, four dependencies. The heavier reviewed
design this was descoped from lives in `docs/superpowers/specs/`.
