#!/usr/bin/env python3
"""Run real agent tasks against iphone-use with `claude -p` and audit them.

Each task is a Markdown file in tasks/ with a small header:

    ---
    name: settings-ios-version
    expect: "iOS \\d+"          # answer format only (optional)
    max_tool_calls: 8            # budget the run is judged against (optional)
    ---
    Open Settings on my iPhone and tell me which iOS version it runs.

The driver gives Claude only the iphone-use MCP server (the installed one by
default), records Claude's stream JSONL, and reports per task: checks, verification, tool
calls by name, failed tool calls, turns, wall time, tokens and cost. This tells
whether the tool descriptions lead a model to the short path, the way
XcodeBuildMCP's and mobile-mcp's agent benchmarks do.

It drives a real phone and spends model tokens, so it is manual, not CI:

    scripts/agent-bench/run.py                 # every task
    scripts/agent-bench/run.py --task settings-ios-version --model claude-sonnet-5-5
    scripts/agent-bench/run.py --from-jsonl out/…/settings-ios-version.jsonl   # re-audit a run

Artifacts go to scripts/agent-bench/out/<timestamp>/ (git-ignored).
"""
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
DEFAULT_MCP = pathlib.Path.home() / "Applications/iPhoneUse.app/Contents/MacOS/iphone-use-mcp"
TOOL_PREFIX = "mcp__iphone-use__"


def parse_task(path):
    """(header dict, prompt) from a task file; the header is optional.

    Header values are plain text, or quoted ("…" / '…') when they hold a
    `#`; anything after an unquoted ` #` is a comment."""
    text = pathlib.Path(path).read_text(encoding="utf-8")
    header = {"name": pathlib.Path(path).stem}
    if text.startswith("---\n"):
        head, _, text = text[4:].partition("\n---\n")
        for line in head.splitlines():
            key, sep, value = line.partition(":")
            if not sep:
                continue
            key, value = key.strip(), value.strip()
            quoted = re.match(r"""^(["'])(.*)\1(\s+#.*)?$""", value)
            value = quoted.group(2) if quoted else re.sub(r"\s+#.*$", "", value)
            header[key] = int(value) if key == "max_tool_calls" else value
    return header, text.strip()


def audit(lines, returncode=0):
    """Summarize one stream-json run (an iterable of JSONL lines). A run
    without a final `result` event, or whose known exit code is non-zero, is
    an error. None means the original exit code was not supplied."""
    calls, failed, names = [], [], {}
    pending = {}
    result = {}
    for raw in lines:
        raw = raw.strip()
        if not raw:
            continue
        try:
            event = json.loads(raw)
        except ValueError:
            continue
        kind = event.get("type")
        content = (event.get("message") or {}).get("content") or []
        if kind == "assistant":
            for block in content:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    name = block.get("name", "")
                    short = name[len(TOOL_PREFIX):] if name.startswith(TOOL_PREFIX) else name
                    calls.append(short)
                    names[short] = names.get(short, 0) + 1
                    pending[block.get("id")] = short
        elif kind == "user":
            for block in content:
                if isinstance(block, dict) and block.get("type") == "tool_result" and block.get("is_error"):
                    failed.append(pending.get(block.get("tool_use_id"), "?"))
        elif kind == "result":
            result = event
    usage = result.get("usage") or {}
    return {
        "tool_calls": len(calls),
        "tools": names,
        "sequence": calls,
        "failed_calls": failed,
        "turns": result.get("num_turns"),
        "duration_ms": result.get("duration_ms"),
        "cost_usd": result.get("total_cost_usd"),
        "input_tokens": usage.get("input_tokens"),
        "output_tokens": usage.get("output_tokens"),
        "answer": result.get("result", ""),
        "is_error": bool(result.get("is_error")) or not result or returncode not in (None, 0),
        "process_returncode": returncode,
    }


def judge(header, summary):
    """Check execution, answer format and call budget, not physical completion."""
    reasons = []
    if summary["is_error"]:
        reasons.append("claude reported an error, exited non-zero or gave no result")
    expect = header.get("expect")
    if expect and not re.search(expect, summary["answer"] or ""):
        reasons.append(f"answer does not match {expect!r}")
    budget = header.get("max_tool_calls")
    if isinstance(budget, int) and summary["tool_calls"] > budget:
        reasons.append(f"{summary['tool_calls']} tool calls > budget {budget}")
    return not reasons, reasons


def assess(header, trace, returncode=None, verification=None):
    """Only an independent, trace-bound receipt can verify task postconditions.

    The model's answer, regex match and tool acknowledgements are not receipts.
    Receipt loading never executes code or takes instructions from the trace.
    """
    digest = hashlib.sha256()
    with open(trace, "rb") as source:
        def captured_lines():
            for line in source:
                digest.update(line)
                yield line.decode("utf-8")
        summary = audit(captured_lines(), returncode)
    fingerprint = digest.hexdigest()
    checks_passed, reasons = judge(header, summary)
    proof = {"state": "unverified"}
    if verification:
        with open(verification, encoding="utf-8") as source:
            receipt = json.load(source)
        if not isinstance(receipt, dict):
            raise ValueError("verification receipt must be an object")
        if receipt.get("task") != header["name"]:
            raise ValueError("verification receipt belongs to another task")
        if receipt.get("trace_sha256") != fingerprint:
            raise ValueError("verification receipt does not match this trace")
        if type(receipt.get("verified")) is not bool:
            raise ValueError("receipt verified must be a boolean")
        for field in ("verifier", "evidence"):
            if not isinstance(receipt.get(field), str) or not receipt[field].strip():
                raise ValueError(f"receipt {field} must name the independent check")
        proof = {key: receipt[key] for key in
                 ("task", "trace_sha256", "verifier", "evidence")}
        proof["state"] = "passed" if receipt["verified"] else "failed"
    if not checks_passed or proof["state"] == "failed":
        passed = False
        if proof["state"] == "failed":
            reasons.append("independent task verification failed")
    elif proof["state"] == "passed" and returncode == 0:
        passed = True
    else:
        passed = None
        if proof["state"] != "passed":
            reasons.append("task postconditions have not been independently verified")
        if returncode is None:
            reasons.append("the original process exit status was not supplied")
    return {"schema_version": 2, "task": header["name"], **summary, "passed": passed,
            "trace_sha256": fingerprint,
            "checks_passed": checks_passed, "verification": proof, "reasons": reasons}


def run_task(path, out_dir, mcp, model):
    header, prompt = parse_task(path)
    config = out_dir / "mcp.json"
    config.write_text(json.dumps({"mcpServers": {"iphone-use": {"command": str(mcp)}}}))
    jsonl = out_dir / f"{header['name']}.jsonl"
    command = [
        "claude", "-p", prompt,
        "--output-format", "stream-json", "--verbose",
        "--mcp-config", str(config), "--strict-mcp-config",
        "--allowedTools", f"{TOOL_PREFIX}*",
    ]
    if model:
        command += ["--model", model]
    started = time.monotonic()
    with open(jsonl, "w", encoding="utf-8") as sink:
        proc = subprocess.run(command, stdout=sink, stderr=subprocess.STDOUT, cwd=out_dir, check=False)
    result = assess(header, jsonl, proc.returncode)
    result["wall_ms"] = int((time.monotonic() - started) * 1000)
    return result


def print_report(rows):
    for row in rows:
        mark = "UNVERIFIED" if row["passed"] is None else "PASS" if row["passed"] else "FAIL"
        elapsed = row.get("duration_ms")
        if elapsed is None:
            elapsed = row.get("wall_ms")
        timing = f"{elapsed / 1000:.1f} s" if elapsed is not None else "duration unknown"
        cost = row.get("cost_usd")
        print(f"{mark} {row['task']}: {row['tool_calls']} tool calls, "
              f"{len(row['failed_calls'])} failed, {row.get('turns')} turns, {timing}"
              + (f", ${cost:.4f}" if isinstance(cost, (int, float)) else ""))
        print(f"     sequence: {' → '.join(row['sequence']) or '(none)'}")
        for reason in row["reasons"]:
            print(f"     ✗ {reason}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--task", action="append", help="task name (tasks/<name>.md); repeatable")
    parser.add_argument("--model", help="exact Claude model id for a controlled comparison")
    parser.add_argument("--mcp", default=os.environ.get("IPHONE_USE_MCP", str(DEFAULT_MCP)))
    parser.add_argument("--from-jsonl", help="re-audit a saved run instead of running Claude")
    parser.add_argument("--verification", help="independent JSON receipt bound to --from-jsonl")
    parser.add_argument("--returncode", type=int, help="original process exit code for --from-jsonl")
    parser.add_argument("--json", action="store_true", help="print machine-readable results")
    args = parser.parse_args(argv)
    replay = args.from_jsonl is not None
    if replay and not args.from_jsonl.strip():
        parser.error("--from-jsonl must name a trace file")
    if args.verification is not None and not args.verification.strip():
        parser.error("--verification must name a receipt file")
    if args.verification is not None and not replay:
        parser.error("--verification requires --from-jsonl")
    if args.returncode is not None and not replay:
        parser.error("--returncode requires --from-jsonl")

    if replay:
        if args.task and len(args.task) != 1:
            parser.error("re-auditing needs exactly one --task")
        name = args.task[0] if args.task else pathlib.Path(args.from_jsonl).stem
        task = HERE / "tasks" / f"{name}.md"
        if not task.is_file():
            parser.error("no matching task header; provide --task NAME")
        header, _ = parse_task(task)
        try:
            rows = [assess(header, args.from_jsonl, args.returncode, args.verification)]
        except (OSError, ValueError) as error:
            parser.error(str(error))
    else:
        tasks = sorted((HERE / "tasks").glob("*.md"))
        if args.task:
            tasks = [HERE / "tasks" / f"{name}.md" for name in args.task]
        if not tasks:
            parser.error("no benchmark tasks were selected")
        out_dir = HERE / "out" / datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
        out_dir.mkdir(parents=True, exist_ok=True)
        rows = [run_task(task, out_dir, args.mcp, args.model) for task in tasks]
        (out_dir / "results.json").write_text(json.dumps(rows, ensure_ascii=False, indent=2))
    if args.json:
        print(json.dumps(rows, ensure_ascii=False, indent=2))
    else:
        print_report(rows)
    if any(row["passed"] is False for row in rows):
        return 1
    return 0 if all(row["passed"] is True for row in rows) else 3


if __name__ == "__main__":
    sys.exit(main())
