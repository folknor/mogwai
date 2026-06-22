@AGENTS.md

## More rules

### Multi-Agent Orchestration

**The spec-loop**: when the user asks to orchestrate, to run the loop, or to
work a goal down to landed commits, read `reference/orchestrate.md` FIRST and
follow it exactly - it is the standing procedure (roles, the seven steps, the
waiting discipline, codex invocation). Note its Input section: confirm the
goal with the user before launching anything. The orchestrate.md workflow,
once invoked, overrides the foreground-subagent rule below (its launches are
background by design, per the user's standing instruction in that document).

**Always get permission from the user before launching subagents - ASK FIRST,
EVERY TIME.** This is not satisfied by the user approving the underlying task.
"Yes, fix the bug" authorizes the work, NOT the fan-out: spawning Agent/Task
subagents (Explore, general-purpose, fork, anything) is a separate decision the
user makes explicitly. Before any `Agent`/`Task` launch, stop and ask in chat -
name what you want to spawn and why - then wait for a yes. Doing the
investigation yourself with Read/Grep/Bash needs no permission; only delegating
to subagents does. The sole exception is the orchestrate.md spec-loop, which the
user invokes by name and which carries its own standing authorization.

**Do NOT use git worktree isolation for parallel agents.** Worktrees create merge conflicts that silently drop agent work. Instead, launch agents in the same tree with strict file ownership - zero overlap.

Agent coordination rules:
- Each agent gets exclusive ownership of specific files. No two agents touch the same file.
- Agents must read their target file FIRST. Do not replace existing code with placeholders or stub it out.
- Agents must NOT run `brokkr check`, `brokkr test`, or `cargo`. The orchestrator validates between agents.

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
- For review tasks, ask for findings labeled *bug* / *gap* / *smell* / *nit* so the orchestrator can triage without re-reading the whole report.

### Communication rules

- Never use the `AskUserQuestion` tool - the harness runs in don't-ask mode and it will be denied. When you need a decision from the user, just ask in chat with the options laid out in prose.

### General rules

- Subagents must always be launched in the foreground, (never use `run_in_background: true`) so the user can approve tool requests.

### Memory rules

Do not use your Memory functionality. Do not read, write, or update memories. Do not suggest saving things to memory. Durable context belongs in CLAUDE.md or the relevant docs.

### Bash rules

- Never use `sed`, `find`, `awk`, `head`, `tail`, or complex bash commands.
- Never `find /`.
- Never run `git` with `-C <path>`
- One Bash() invocation === one command
- Keep `git commit -m` messages free of zsh metacharacters - braces `{}`, brackets `[]`, parens `()`, angle brackets `<>`, `#`. They trip the permission matcher and block the commit. Spell lists out (`syntax, vm, data and runner`, not `{syntax,vm,data,runner}`), write `5.1 per bar` not `5.1/bar`, name attributes in prose not `#[attr]`.
