#!/usr/bin/env python3
"""Minimal stream-json mock of `claude` for UI testing.

Usage: mock_claude.py <repo>   (reads JSON lines on stdin, writes JSON lines on stdout)
- message containing "Annotate" -> JSON annotations for lines 1..120
- message containing "PERM"     -> sends a can_use_tool control_request first
- message containing "CRASH"    -> exits with an error
- message containing "LONG"     -> streams a long markdown answer
- anything else                 -> streams an echo answer in markdown
"""
import json
import sys
import time

repo = sys.argv[1] if len(sys.argv) > 1 else "?"


def out(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()


out({"type": "system", "subtype": "init", "session_id": "mock", "model": "mock-model", "permissionMode": "default", "cwd": repo})
n = 0
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") != "user":
        continue
    n += 1
    text = msg["message"]["content"]
    sys.stderr.write(f"[mock] got message {n}: {text[:80]!r}\n")
    if "CRASH" in text:
        sys.stderr.write("mock: simulated crash\n")
        sys.exit(3)
    if "PERM" in text:
        out({"type": "control_request", "request_id": f"req-{n}", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "rm -rf /tmp/nothing", "description": "Mock dangerous command"}, "description": "Mock dangerous command", "permission_suggestions": [{"type": "addRules"}], "decision_reason": "mock"}})
        resp = json.loads(sys.stdin.readline())
        decision = resp["response"]["response"]["behavior"]
        answer = f"Permission decision received: **{decision}**."
    elif "LONG" in text:
        answer = f"Long echo #{n}.\n\n" + "\n".join(f"{i}. Point number {i}, padded out so that the reply wraps and takes real vertical space in the chat log." for i in range(1, 41))
    elif "Annotate" in text:
        ann = {str(i): f"Mock annotation for line {i}: " + ("this is a long explanation that keeps going " * (1 + i % 3)) for i in range(1, 121)}
        answer = json.dumps({"annotations": ann})
    else:
        answer = f"Echo #{n} from mock in `{repo}`.\n\n- first point\n- second point with `code`\n\n```rust\nfn main() {{}}\n```"
    mid = f"msg-{n}"
    out({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "message_start", "message": {"id": mid}}})
    if "Annotate" not in text:
        out({"type": "assistant", "parent_tool_use_id": None, "message": {"id": mid + "-t", "content": [{"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "src/lib.rs"}}]}})
        for chunk in [answer[i:i + 12] for i in range(0, len(answer), 12)]:
            out({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": chunk}}})
            time.sleep(0.02)
    out({"type": "assistant", "parent_tool_use_id": None, "message": {"id": mid, "content": [{"type": "text", "text": answer}]}})
    out({"type": "result", "subtype": "success", "is_error": False, "result": answer, "total_cost_usd": 0.001 * n})
