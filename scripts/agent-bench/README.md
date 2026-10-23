# Agent benchmark

Runs real tasks with `claude -p` against the installed `iphone-use-mcp` and
audits each run: execution checks, independent verification, tool calls by name
and in order, failed calls,
turns, time, tokens and cost. Use it to check that the tool descriptions lead a
model down the short path. Run it after you change a tool description, the
skill, or an output format, and compare the results with an earlier run.

It drives a real iPhone and spends model tokens, so it is a manual check and
does not run in CI.

A final answer, matching regex or successful API acknowledgement does not verify
the task. Runs without an independent postcondition check report `UNVERIFIED`.
Metrics remain available for those runs; they are not successful-task results.

```bash
scripts/agent-bench/run.py                                   # every task in tasks/
scripts/agent-bench/run.py --task settings-ios-version --model claude-sonnet-5-5
scripts/agent-bench/run.py --from-jsonl scripts/agent-bench/out/<run>/<task>.jsonl
```

- Claude gets only the iphone-use MCP server (`--strict-mcp-config`), and only
  `mcp__iphone-use__*` tools are allowed.
- `--mcp <path>` (or `IPHONE_USE_MCP`) picks another build.
- Artifacts go to `out/<timestamp>/` (git-ignored): the stream JSONL per task
  and `results.json`.
- Offline replay applies the same task header as a live run. It infers the task
  from the trace filename, or accepts one explicit `--task NAME`. An unmatched
  trace cannot silently bypass the task rules.

A task is a Markdown prompt with an optional header:

```markdown
---
name: settings-ios-version
expect: "\d+\.\d+"   # answer format only; not proof of the actual version
max_tool_calls: 8    # call budget; not proof of task completion
---
Open Settings on my iPhone and tell me which iOS version it runs.
```

Keep tasks read-only or reversible. They run on a real phone.

After a separate harness or a person checks the task's postconditions, save a
receipt outside the model trace:

```json
{
  "task": "settings-ios-version",
  "trace_sha256": "<SHA-256 of settings-ios-version.jsonl>",
  "verified": true,
  "verifier": "independent-device-assertions",
  "evidence": "/path/to/postcondition-results.json"
}
```

The receipt records an independent check; this driver does not perform that
check itself. The check must cover both the requested answer and the on-device
postconditions. Do not generate it from the agent's answer or a tool's `ok:true`.
The exact task and trace digest must match. A failed check uses `verified:false`.

```bash
scripts/agent-bench/run.py --from-jsonl out/run/settings-ios-version.jsonl \
  --task settings-ios-version --returncode 0 --verification verification.json
```

Use the original process exit code, recorded as `process_returncode` in the live
run's `results.json`. If it is unknown for an old trace, omit `--returncode` and
keep the result unverified. Do not invent a zero exit code to obtain a pass.

Result schema 2 separates `checks_passed` from `verification`. `passed` is `null`
when verification or the original exit status is missing. Exit codes are 0 for
verified passes, 1 for failed checks or postconditions, 2 for invalid arguments
or receipts, and 3 for unverified results. Compare runs with the same model,
device, task, initial state and verification procedure.
