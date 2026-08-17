# Scopic Backlog

Only unfinished work belongs here. Items are ordered roughly by usefulness and
cost, not promised release order.

## Near-term hardening

### 1. Persist reviewed files for a diff [M]

Add a GitHub-style "viewed" control to file headers. Marking a file viewed
should collapse it and persist for the same diff identity, while ordinary
collapse remains temporary. Decide the stable diff hash and storage location
before implementation.

## Review workflow

### 2. Attach comments to diff lines [L]

Allow review notes on individual left/right lines without turning scopic into
an editor. The first useful version should support creating, editing, listing,
and exporting comments in a format a coding agent can consume. Persistence and
line identity must survive viewport changes and collapsed files.

### 3. Expand diff hunks with repository context [L]

Offer GitHub-style expansion above or below a hunk by a small line count. Piped
diff text does not contain the missing source, so this requires an optional
repository-aware input path and clear behavior for deleted, renamed, binary,
or unavailable files. Keep ordinary stdin/file diff viewing standalone.

## Optional polish

### 4. Remember pane widths per file [M]

Let a file keep its own divider ratio instead of applying one ratio to the
whole diff. Revisit only if global resizing remains limiting now that files are
independently collapsible.

### 5. Improve divider discoverability [S]

Consider a hover/drag highlight or another terminal-native affordance if users
miss the draggable divider. Terminal applications cannot reliably request a
GUI `col-resize` cursor, so avoid terminal-specific cursor protocols unless a
portable option appears.
