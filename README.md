# Reviewfren

A Rust/Iced desktop app for reviewing GitHub pull requests locally, with Claude Code
built in.

```
cargo run --release                 # start on the repository picker
cargo run --release -- /path/repo   # jump straight to the PR list of a repo
```

Requirements: `git`, an authenticated `gh`, and `claude`. Language servers are fetched
from the Mason registry on demand; `gopls` additionally needs `go` on `PATH`.

## Workflow

1. **Repository picker**: "Choose repository…" opens the system folder dialog
   (xdg-desktop-portal). You can also type or paste a path, or pick a recent repo.
   If the folder isn't a git repo, or `gh pr list` fails, you return here with a red
   error box.
2. **PR list**: open PRs from `gh pr list`. Each layer of a stacked PR (a PR whose
   base branch is another open PR's head branch) gets a `☰ stack i/n` badge.
   Clicking a PR first checks `git status --porcelain --untracked-files=no`. If
   there are uncommitted changes, you stay on the list with an error box.
3. **Checkout**: `git fetch --prune` → `git checkout <head>` → `git pull --ff-only` →
   `git fetch --prune`. The extra fetch at the start lets a branch that only exists
   on the remote be checked out. If the checkout fails, for example because the PR
   comes from a fork, it falls back to `gh pr checkout <n>`.
4. **Diff**: merge base = `git merge-base HEAD <remote>/<base>`. Changed files come
   from `git diff --numstat -z -M <mb>`, the machine-readable form of `--stat`, so
   long paths aren't truncated. Each file is then diffed with
   `git diff <mb> -- <file>`.
5. **Review page**: a continuous page with one collapsible section per file:
   * a header with the path, a copy-path button, added/deleted/renamed badges,
     `+N −M` and a stat bar, a **Full file / Compact** toggle, a **↩ Wrap** toggle
     (overrides the global default for that file), the **✦ Claude** menu and a
     **Viewed** checkbox;
   * a syntax-highlighted diff with two line-number gutters and GitHub's red/green
     rows, with the words that actually changed in a darker shade. Each block of
     removed + added lines is diffed token by token with `similar`; lines that are
     mostly rewritten get no word highlights. The full-file view shows the whole new file with removed lines
     interleaved. Old and new versions are each highlighted as whole files, so
     multi-line strings and comments stay correct;
   * review comments, like GitHub's: each thread sits under the line it is on, with
     the commenters' avatars and a comment count at the end of the line (click to
     show or hide it). Open threads start expanded, resolved ones collapsed. Threads
     are loaded with `gh api graphql` (`reviewThreads`); outdated ones are left out,
     as on GitHub's "Files changed" tab. Replies and resolving happen on GitHub
     (**Reply on GitHub ↗**). In the compact view, a banner counts threads on lines
     outside the shown hunks;
   * Viewed collapses the section, greys it out in the tree, and persists in
     `~/.local/share/reviewfren/viewed.json`. A mark is keyed by a hash of the
     file's diff, so it resets when the file changes.
   * left: a toggleable file tree with a text filter and extension filters
     (filters apply to the page too). Click a file to scroll to it.
   * right: a toggleable general Claude chat panel.
   * bottom: a status bar showing Claude status, model, permission mode, session
     and cost, plus each LSP's status and progress.

Code view interactions: drag to select, double-click to select a word,
triple-click to select a line, Shift+click to extend, Ctrl+C to copy, Esc to
clear. Long lines either scroll horizontally (Shift+wheel, a horizontal wheel, or
the scrollbar, which sticks to the bottom of the window while the end of the diff
is off-screen) or soft-wrap, breaking after a space where possible. Right-click opens a
menu with **Go to definition** and **Copy**; with no selection it selects the word
under the cursor first.

## Claude

The `claude` process starts when the review page opens: `sh -c '<command>'` in the
repo directory, talking stream-json over stdin/stdout. It is killed (as a process
group) when you leave the page. All chats and annotation jobs share one session.
Requests are queued, and each `result` is routed back to the chat box or file that
asked for it.

* **✦ Claude → Annotate** asks for a JSON object of one end-of-line comment per
  line of the new file. Comments are drawn after the code, never written into
  files. They're aligned in a column, truncated to fit, and the full text shows on
  hover. Removed lines have no annotation because the keys are new-file line
  numbers.
* **✦ Claude → Chat** opens a chat box under the file. Messages get wrapped in the
  per-file meta prompt; the general panel uses its own meta prompt. Meta prompts
  are never shown in the chat logs. Replies stream in as markdown; tool calls show
  as `⚙` lines; **■ Stop** sends an `interrupt` control request. Chat text can be
  selected (drag, double-click a word, triple-click a line) and copied with Ctrl+C;
  a selection stays within one block (a paragraph, list item or code block).
* Permission prompts (`control_request` / `can_use_tool`) open a modal showing the
  tool, its description and input. **Allow**, **Always allow** (uses Claude's
  `permission_suggestions`) and **Deny** send the matching `control_response`.
  Other control request subtypes get an error reply.

### Settings (⚙, saved to `~/.config/reviewfren/settings.json`)

* **Claude command**: the full shell command. `{repo}` expands to the shell-quoted
  repo path. Default:
  `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio --permission-mode default --disallowedTools AskUserQuestion`.
  For Docker, use `-i`, **not** `-it`. stdin is a pipe, so `-t` fails with "the
  input device is not a TTY". For example:
  `docker exec -i -w {repo} dockerclaude claude -p --input-format stream-json …`
  (the repo has to be mounted at the same path inside the container), or
  `docker exec -i dockerclaude bash -c 'cd /src/repo && claude -p …'`.
  Changing the command restarts Claude.
* **Meta prompts** for per-file chat, general chat and Annotate. Placeholders:
  `{message} {path} {merge_base} {pr_number} {pr_title} {head_branch} {base_branch}`.
* **Wrap long lines**: the default wrap mode for diffs.
* **Appearance**: dark theme, UI font, code font, code font size and UI zoom. The
  font pickers list installed families (type to search; monospace fonts come first
  for code), and a preview shows the result. Code font, size and zoom apply on
  Save. iced only reads the UI font at startup, so changing it turns Save into
  **Save & restart**, which relaunches the app on the current repository. Unknown
  family names fall back to the default font, so a typo can't break the
  monospace grid.

## LSP

The **LSP** menu starts `rust-analyzer` or `gopls` with the repo root as its
workspace. Binaries are looked up in this order:

1. the app's own Mason dir, `~/.local/share/reviewfren/mason/bin`;
2. Neovim's Mason dir, `~/.local/share/nvim/mason/bin`;
3. a fresh install from the Mason registry: GitHub release assets for
   rust-analyzer, `go install` for gopls;
4. `PATH`, as a fallback if the install fails.

`$/progress` and rust-analyzer's `experimental/serverStatus` feed the status bar.
**Go to definition** sends `textDocument/didOpen` plus `textDocument/definition`
for the first identifier in the selection. The raw response is printed to stdout,
and a short `path:line` summary appears in the status bar. It only works on lines
that exist in the PR's version: context or added lines. Note that rust-analyzer
runs `cargo check`, so it creates `target/` in the reviewed repo.

## Code map

| file | purpose |
| --- | --- |
| `src/app.rs` | state, messages, update logic, Claude job queue, scroll-to-file operation |
| `src/view.rs` | all screens and overlays (modals, context menu, file tree, chats, status bar) |
| `src/code_view.rs` | custom diff/code widget: drawing, selection, annotations, tooltips, inserted comment threads |
| `src/selectable_text.rs` | selectable rich text, used for markdown in chats and the PR description |
| `src/diff.rs` | unified diff parsing, compact/full row models, changed-word ranges, tab and column mapping |
| `src/highlight.rs` | syntect highlighting (two-face syntaxes/themes) |
| `src/git.rs` | `git`/`gh` wrappers, stack detection, review comment threads |
| `src/claude.rs` | `claude` subprocess and stream-json protocol |
| `src/lsp.rs` | minimal JSON-RPC LSP client |
| `src/mason.rs` | Mason registry lookup and installs |
| `src/settings.rs` | settings, prompt templates, viewed-state store |
| `src/fonts.rs` | installed font families, active UI/code fonts |

## Development

```
cargo test                                  # unit tests
cargo test -- --ignored gopls_definition    # LSP integration test (needs go + network)
```

`dev/mock_claude.py` is a small stream-json stand-in for `claude`, useful for UI
work that shouldn't spend tokens. Set the Claude command to
`python3 /path/to/dev/mock_claude.py {repo}`. Messages containing `Annotate`,
`PERM`, `CRASH` or `LONG` trigger annotations, a permission prompt, a crash or a long
reply.

`dev/harness.sh` runs the app against a fixture instead of a real repository, under
Xvfb, so it can be driven with `xdotool` and screenshotted (needs Xvfb, xdotool and
ImageMagick). `setup` creates a local repo whose remote has a `feature` branch;
`open` starts the app with a fake `gh` on `PATH` and the mock Claude, and opens the
fixture PR, which has a few review comment threads (`dev/harness/threads.json`);
`shot` takes a screenshot. Run it without arguments for the details.
