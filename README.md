# scopic

*-scopic*, from the Greek *skopein*, "to look at", is the suffix of viewing
instruments: telescopic, microscopic, stereoscopic. This one is for diffs.

A side-by-side terminal viewer for git and jj diffs, styled after the GitHub
split view: per-file header bars with `+n -n` change counts, muted
GitHub-dark colors, full-width hunk bands, intraline word highlights, dual
line numbers.

```
jj diff --git | scopic
git diff | scopic
scopic changes.diff
```

Colored input is fine; ANSI codes are stripped before parsing. Anything
scopic doesn't understand renders as dim text rather than erroring. Empty
input exits silently.

## Keys

`j/k` scroll · `ctrl-d/u` half-page · `n/p` next/prev file · `g/G` top/bottom · `q` quit

## Install & hook up

```
cargo install --path .
```

**jj**, in `~/.config/jj/config.toml`, scoped so `jj log` etc. keep the
normal pager:

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

Two notes on looks:

- The palette uses truecolor. Terminals without RGB support (e.g. tmux
  without `Tc`) quantize the tints toward gray.
- Hunk headers show whatever context the diff carries. `git diff` adds
  function names (better with `*.rs diff=rust` in `.gitattributes`);
  `jj diff --git` doesn't emit them yet.

Internals: [docs/internals.md](docs/internals.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
