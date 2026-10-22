#!/usr/bin/env python3
"""The agent benchmark's audit and judging, against a recorded stream-json run."""
import importlib.util
import contextlib
import hashlib
import io
import json
import pathlib
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("agent_bench", HERE / "agent-bench" / "run.py")
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


def forbid_live_task(*args, **kwargs):
    raise AssertionError("offline audit tests must never start a real agent or phone task")


bench.run_task = forbid_live_task

with open(HERE / "fixtures" / "agent-bench-run.jsonl", encoding="utf-8") as source:
    summary = bench.audit(source)
assert summary["tool_calls"] == 3, summary
assert summary["sequence"] == ["phone_status", "phone_run_steps", "phone_run_steps"], summary
assert summary["tools"] == {"phone_status": 1, "phone_run_steps": 2}, summary
assert summary["failed_calls"] == ["phone_run_steps"], summary
assert summary["turns"] == 4 and summary["duration_ms"] == 12345, summary
assert summary["input_tokens"] == 1000 and summary["output_tokens"] == 200, summary
assert summary["answer"] == "Your iPhone runs iOS 18.1."

passed, reasons = bench.judge({"expect": r"\d+\.\d+", "max_tool_calls": 8}, summary)
assert passed and not reasons, reasons
passed, reasons = bench.judge({"expect": "Android", "max_tool_calls": 2}, summary)
assert not passed and len(reasons) == 2, reasons

# A run with no result event, or a failed claude, never passes, even for a
# task with no expect rule.
lines = open(HERE / "fixtures" / "agent-bench-run.jsonl", encoding="utf-8").read().splitlines()
cut = bench.audit([l for l in lines if '"type":"result"' not in l.replace(" ", "")])
assert cut["is_error"], cut
assert not bench.judge({}, cut)[0]
crashed = bench.audit(lines, returncode=1)
assert crashed["is_error"] and not bench.judge({}, crashed)[0], crashed
assert not bench.audit(lines)["is_error"]

with tempfile.TemporaryDirectory() as tmp:
    task = pathlib.Path(tmp) / "t.md"
    task.write_text('---\nname: demo\nexpect: "a#b"   # comment\nmax_tool_calls: 5 # budget\n---\nDo it.\n')
    header, prompt = bench.parse_task(task)
    assert header == {"name": "demo", "expect": "a#b", "max_tool_calls": 5}, header
    assert prompt == "Do it."
    plain = pathlib.Path(tmp) / "plain.md"
    plain.write_text("Just a prompt.\n")
    assert bench.parse_task(plain) == ({"name": "plain"}, "Just a prompt.")

for task in sorted((HERE / "agent-bench" / "tasks").glob("*.md")):
    header, prompt = bench.parse_task(task)
    assert header["name"] == task.stem and prompt, task

with tempfile.TemporaryDirectory() as tmp:
    trace = pathlib.Path(tmp) / "settings-ios-version.jsonl"
    original = "\n".join(lines) + "\n"
    trace.write_text(original)
    header, _ = bench.parse_task(HERE / "agent-bench/tasks/settings-ios-version.md")
    row = bench.assess(header, trace, returncode=0)
    assert row["checks_passed"] and row["passed"] is None, row
    assert row["verification"]["state"] == "unverified", row
    display = io.StringIO()
    with contextlib.redirect_stdout(display):
        bench.print_report([{**row, "duration_ms": None, "wall_ms": None}])
    assert "duration unknown" in display.getvalue()
    assert "0.0 s" not in display.getvalue()
    assert row["trace_sha256"] == hashlib.sha256(trace.read_bytes()).hexdigest()

    receipt = {"task": header["name"], "trace_sha256": row["trace_sha256"],
               "verified": True, "verifier": "independent-fixture-assertions",
               "evidence": "synthetic UI postconditions matched"}
    proof = pathlib.Path(tmp) / "verification.json"
    proof.write_text(json.dumps(receipt))
    assert bench.assess(header, trace, 0, proof)["passed"] is True
    assert bench.assess(header, trace, 1, proof)["passed"] is False
    assert bench.assess(header, trace, verification=proof)["passed"] is None
    proof.write_text(json.dumps({**receipt, "verified": False}))
    assert bench.assess(header, trace, 0, proof)["passed"] is False

    for overrides in [{"task": "notes-search"}, {"trace_sha256": "0" * 64},
                      {"verified": "true"}, {"verified": 1},
                      {"verifier": ""}, {"evidence": ""}]:
        proof.write_text(json.dumps({**receipt, **overrides}))
        try:
            bench.assess(header, trace, 0, proof)
            raise AssertionError(f"invalid receipt accepted: {overrides}")
        except ValueError:
            pass
    proof.write_text(json.dumps(receipt))
    trace.write_text(original + "\n")
    try:
        bench.assess(header, trace, 0, proof)
        raise AssertionError("a receipt for a different trace was accepted")
    except ValueError:
        pass
    trace.write_text(original)

    def replay(*extra):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = bench.main(["--from-jsonl", str(trace), "--json", *extra])
        return code, json.loads(output.getvalue())[0]

    code, row = replay("--returncode", "0")
    assert code == 3 and row["passed"] is None
    code, row = replay("--returncode", "0", "--verification", str(proof))
    assert code == 0 and row["passed"] is True
    code, row = replay("--verification", str(proof))
    assert code == 3 and row["process_returncode"] is None

    # Offline judging must apply the same task format and call budget.
    trace.write_text(original.replace("Your iPhone runs iOS 18.1.", "Android"))
    code, row = replay("--returncode", "0")
    assert code == 1 and row["passed"] is False and not row["checks_passed"]
    events = [json.loads(line) for line in lines if line != "not json at all"]
    final = events.pop()
    for i in range(6):
        call_id = f"extra-{i}"
        events.extend([
            {"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": call_id,
                 "name": "mcp__iphone-use__phone_status", "input": {}}]}},
            {"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": call_id, "content": "ready"}]}},
        ])
    events.append(final)
    trace.write_text("\n".join(json.dumps(event) for event in events) + "\n")
    code, row = replay("--returncode", "0")
    assert code == 1 and row["tool_calls"] == 9 and not row["checks_passed"]

    # A model's claim of verification is not an independent receipt.
    events = [json.loads(line) for line in lines if line != "not json at all"]
    events[-1]["verified"] = True
    trace.write_text("\n".join(json.dumps(event) for event in events) + "\n")
    code, row = replay("--returncode", "0")
    assert code == 3 and row["passed"] is None

    for args in [["--verification", str(proof)],
                 ["--from-jsonl", ""], ["--from-jsonl", "   "],
                 ["--verification", ""],
                 ["--from-jsonl", str(trace), "--verification", ""],
                 ["--from-jsonl", str(trace), "--task", "notes-search", "--task", "home-screen-apps"],
                 ["--from-jsonl", str(trace), "--task", "missing-task"]]:
        with contextlib.redirect_stderr(io.StringIO()):
            try:
                bench.main(args)
                raise AssertionError(f"invalid replay options accepted: {args}")
            except SystemExit as error:
                assert error.code == 2

    old_here = bench.HERE
    try:
        bench.HERE = pathlib.Path(tmp) / "empty-benchmark"
        with contextlib.redirect_stderr(io.StringIO()):
            try:
                bench.main([])
                raise AssertionError("an empty benchmark passed")
            except SystemExit as error:
                assert error.code == 2
    finally:
        bench.HERE = old_here

print("agent-bench: ok")
