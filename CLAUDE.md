@AGENTS.md

## More rules

### Multi-Agent Orchestration

**Always get permission from the user before launching subagents - ask first,
every time.** This is not satisfied by the user approving the underlying task.
"Yes, fix the bug" authorizes the work, NOT the fan-out: spawning Agent/Task
subagents (Explore, general-purpose, fork, anything) is a separate decision the
user makes explicitly. Before any `Agent`/`Task` launch, stop and ask in chat -
name what you want to spawn and why - then wait for a yes. Doing the
investigation yourself with Read/Grep/Bash needs no permission; only delegating
to subagents does. The sole exception is the orchestrate spec-loop, which the
user invokes by name and which carries its own standing authorization.

**Do NOT use git worktree isolation for parallel agents.** Worktrees create merge conflicts that silently drop agent work. Instead, launch agents in the same tree with strict file ownership - zero overlap.

Agent coordination rules:
- Each agent gets exclusive ownership of specific files. No two agents touch the same file.
- Agents must read their target file first, before anything else. Do not replace existing code with placeholders or stub it out.
- Agents must NOT run `brokkr` or `cargo`. The orchestrator validates between agents.

Audit protocol:
- Do not trust agent claims of completion. Verify existence + wiring + behavior.
- Use the 3-pass audit structure: domain-specific verification, then cross-cutting reconciliation (does the new instruction actually dispatch? is the new builtin actually installed?), then editorial normalization.
- Any discrepancies doc should contain only current gaps, not historical records. Remove resolved items entirely.

Subagent prompt rules:
- Scope the investigation, not the report. Caps like "under 1500 chars" or "max 15 findings" throw away signal you asked them to surface.
- Invite lateral findings up front. If they notice a bug, optimization, smell, or anything surprising while doing the scoped work, they should flag it, even when it's outside the immediate task.
- Name the question, not the method. Don't prescribe tools ("use `git diff`", "use `Read`"), don't prescribe steps ("read in full, not just hunks"), don't enumerate files when the scope already implies them ("piners-syntax crate only" + the agent's own `ls` / `git diff --name-only` is enough). Prescribing the method wastes tokens and signals distrust.
- Don't restate rules the agent already inherits. Subagents load the same CLAUDE.md / AGENTS.md as the main session, so the bash rules, no-cargo, no-worktrees, gremlins, etc. are already in scope. Re-listing them is noise.
- Do pass anything learned in *this* conversation that the agent can't see: the user's framing, prior decisions, what's already been ruled out, the specific claim being audited.

### Process reset, 2026-08-12 (owner ruling after the churn week)

A week of consensus-gated autonomy produced 201 commits and zero tape
improvement. The mechanism: a consensus gate between a proposer and a
verifier-with-refusal-power converges to the verifier's utility
function, and Codex's is never-being-wrong, which is always achievable
by measuring more. The standing corrections:

- **Codex is scoped to correctness review** - implementation review,
  bug hunting, claims about completed work. It does NOT gate program
  direction, scope, approach selection, or whether to build. Direction
  belongs to the owner and the orchestrating Claude.
- **Every demand for measurement - from anyone - must name the
  decision the result would change.** No decision named, no
  measurement run.
- **A rendered chart, eyeballed by the owner, is a standing gate** for
  any change to tape generation. It is the cheapest gate available and
  the only one that catches what statistics cannot see.
- **The budget is minutes of owner attention at real forks.** Autonomy
  that consumes more of the owner than it saves is worse than none.
  Do not manufacture forks; do not present menus where a
  recommendation suffices; when the owner is depleted, park cleanly.

### Communication rules

- Never use the `AskUserQuestion` tool - the harness runs in don't-ask mode and it will be denied. When you need a decision from the user, just ask in chat with the options laid out in prose.
- Never offer to commit or tell the user "per your rules I've left things uncommitted". Don't mention git commits, ever. The user will instruct you when to commit.

### Memory rules

Do not use your Memory functionality. Do not read, write, or update memories. Do not suggest saving things to memory. Durable context belongs in CLAUDE.md or the relevant docs.

### Bash rules

- Never chain commands with `&&`.
- Never chain commands with `;`.
- Never chain/pipe commands with `|`.
- Never capture stdout into env vars (`UUID=$(...)`).
- Never use `sed`, `find`, `awk`, `head`, `tail`, or complex bash commands.
- Never `find /`.
- Never run `git` with `-C <path>`
- One Bash() invocation === one command
- A long command that is still running is not a problem to solve. Never start a
  second one to find out how the first is going, and never poll its output file.
  This applies to commands the user backgrounds too: the completion
  notification arrives on its own. `brokkr` takes a workspace lock, so a second
  invocation just waits on the first anyway.
- Keep `git commit -m` messages free of zsh metacharacters - braces `{}`, brackets `[]`, parens `()`, angle brackets `<>`, `#`. They trip the permission matcher and block the commit. Spell lists out (`syntax, vm, data and runner`, not `{syntax,vm,data,runner}`), write `5.1 per bar` not `5.1/bar`, name attributes in prose not `#[attr]`.

### git commit rules

These live here rather than in AGENTS.md because only you commit - codex agents
never do, so the rules were dead weight in the file both of you read.

- Always run `brokkr fmt` before a commit.
- Run `brokkr check --gate`, not plain `brokkr check`, before a commit that
  touches `mogwai-adapter`. The plain check cannot see the four socket-backed
  adapter test binaries; two regressions have shipped red through that gap.
- Never commit markdown changes alone. Bundle them with upcoming code commits.
- When committing other changes: always tag along markdown files if dirty.
- Write substantive engineering-focused commit messages.
- Hard-wrap the message body at ~72 columns, matching the existing history; the
  subject stays one concise line. The wall-of-text we keep producing comes from
  `git commit -m "<whole paragraph>"`: a single `-m` is recorded as ONE unwrapped
  line. Embed real line breaks so every body line wraps at ~72 (one `-m` per
  paragraph is fine only when each paragraph already carries its own newlines).
  Newlines are not metacharacters, so this composes with the Bash rules above -
  wrap with literal newlines while still avoiding braces, brackets, parens,
  angle brackets and the hash sign.
- Has `Cargo.lock` changed? Commit it.
- Never `git push` unless the user explicitly asks. Stop after the commit.
