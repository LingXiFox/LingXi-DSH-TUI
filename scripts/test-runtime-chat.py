#!/usr/bin/env python3
"""Exercise the compiled TUI and its pinned SDK on a Unix PTY without credentials."""

import errno
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import unicodedata


ROOT = Path(__file__).resolve().parents[1]
FILE_TEXT = "Official-runtime-read-marker"
CHILD_PROMPT = "Return the child fixture marker."
CHILD_REPLY = "Child fixture marker"
ROWS, COLS = 32, 132
ESCAPE = re.compile(r"\x1b\[([0-9;?]*)([@-~])")


def screen_text(output):
    """Apply the cursor moves and clearing commands used by the test's backend."""
    cells = [[" "] * COLS for _ in range(ROWS)]
    row = col = index = 0
    text = output.decode(errors="ignore")
    while index < len(text):
        if match := ESCAPE.match(text, index):
            args, command = match.groups()
            if command in "Hf":
                values = [int(value or 1) for value in args.split(";")]
                row, col = values[0] - 1, (values[1] if len(values) > 1 else 1) - 1
            elif command == "J" and args == "2" or command == "h" and args == "?1049":
                cells = [[" "] * COLS for _ in range(ROWS)]
            index = match.end()
            continue
        char = text[index]
        index += 1
        if char == "\x1b":
            break
        if char == "\r":
            col = 0
        elif char == "\n":
            row = min(row + 1, ROWS - 1)
        elif char.isprintable() and 0 <= row < ROWS and 0 <= col < COLS:
            cells[row][col] = char
            col += 2 if unicodedata.east_asian_width(char) in "WF" else 1
    return "\n".join("".join(row) for row in cells)


def stream(content, stop, index):
    start = {"id": f"local-{index}", "model": "deepseek-v4-pro",
             "usage": {"input_tokens": 3, "output_tokens": 0}}
    events = [
        {"type": "message_start", "message": start},
        {"type": "content_block_start", "index": 0, "content_block": content},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 1}},
        {"type": "message_stop"},
    ]
    return "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events).encode()


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.requests.append(request)
        index = len(self.server.requests)
        if index == 1:
            content = {"type": "tool_use", "id": "fixture-read", "name": "read",
                       "input": {"file_path": "fixture.txt"}}
            stop = "tool_use"
        elif index in (4, 6):
            marker = "denied.txt" if index == 4 else "allowed.txt"
            content = {"type": "tool_use", "id": f"fixture-approval-{index}", "name": "bash",
                       "input": {"description": "Write a synthetic approval marker",
                                 "command": f"printf approval-marker > {marker}",
                                 "sandbox_permissions": "danger-full-access",
                                 "justification": f"Check the approval round trip for {marker}."}}
            stop = "tool_use"
        elif index == 8:
            content = {"type": "tool_use", "id": "fixture-question", "name": "ask_user_question",
                       "input": {"questions": [{"id": "fixture-choice", "question": "Pick fixture options",
                                                "multi_select": True,
                                                "options": [{"label": "Alpha"}, {"label": "Beta"}]}]}}
            stop = "tool_use"
        elif index == 10:
            content = {"type": "tool_use", "id": "fixture-child", "name": "subagent",
                       "input": {"description": "Check child session", "prompt": CHILD_PROMPT}}
            stop = "tool_use"
        elif any(message["role"] == "user" and any(
            item.get("type") == "text" and item.get("text") == CHILD_PROMPT
            for item in message["content"] if isinstance(item, dict)
        ) for message in request["messages"]):
            content = {"type": "text", "text": CHILD_REPLY}
            stop = "end_turn"
        else:
            content = {"type": "text", "text": f"Confirmed response {index}"}
            stop = "end_turn"
        body = stream(content, stop, index)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def child(binary, root, work, port):
    try:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        before = termios.tcgetattr(0)
        env = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL", "TMPDIR") if key in os.environ}
        env.update(TERM="xterm-256color", HOME=str(work), DSH_TUI_ROOT=str(root),
                   DEEPSEEK_API_KEY="local-fixture", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}")
        process = subprocess.Popen([binary], cwd=work, env=env)
        print(f"TUI_PID={process.pid}", flush=True)
        status = process.wait()
        if status == 0 and termios.tcgetattr(0) == before:
            print("TTY_RESTORED", flush=True)
            os._exit(0)
    except BaseException as error:
        print(f"PTY child failed: {error}", flush=True)
    os._exit(1)


def process_table():
    output = subprocess.check_output(["ps", "-axo", "pid=,ppid="], text=True)
    return {int(pid): int(parent) for pid, parent in (line.split() for line in output.splitlines())}


def cpu_seconds(pid):
    value = subprocess.check_output(["ps", "-p", str(pid), "-o", "time="], text=True).strip()
    seconds = 0.0
    for part in value.split(":"):
        seconds = seconds * 60 + float(part)
    return seconds


def exercise(binary, root, work, server):
    wrapper, terminal = pty.fork()
    if wrapper == 0:
        child(binary, root, work, server.server_port)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    output = bytearray()
    exited = False

    def expect(text, *, raw=False):
        def matches():
            if raw:
                return text.encode() in output
            screen = screen_text(output)
            return text.search(screen) is not None if isinstance(text, re.Pattern) else text in screen

        deadline = time.monotonic() + 25
        while not matches() and time.monotonic() < deadline:
            if select.select([terminal], [], [], 0.1)[0]:
                try:
                    chunk = os.read(terminal, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    break
                if not chunk:
                    break
                output.extend(chunk)
        if not matches():
            raise AssertionError(f"PTY did not show {text!r}:\n{screen_text(output)}")

    try:
        expect("ready")
        tui = next(pid for pid, parent in process_table().items() if parent == wrapper)
        runtime = next(pid for pid, parent in process_table().items() if parent == tui)
        if os.environ.get("DSH_TEST_IDLE") == "1":
            expect("Enter a prompt")
            while select.select([terminal], [], [], 0.1)[0]:
                output.extend(os.read(terminal, 65536))
            start = time.monotonic()
            tui_cpu = cpu_seconds(tui)
            runtime_cpu = cpu_seconds(runtime)
            assert not select.select([terminal], [], [], 5)[0], "Idle TUI repainted"
            elapsed = time.monotonic() - start
            print(f"Idle {elapsed:.2f}s: TUI {(cpu_seconds(tui) - tui_cpu) / elapsed:.2%} CPU, "
                  f"SDK {(cpu_seconds(runtime) - runtime_cpu) / elapsed:.2%} CPU, zero terminal writes")
        if os.environ.get("DSH_TEST_LATENCY") == "1":
            expect("Enter a prompt")
            samples = []
            for length in range(1, 51):
                start = time.monotonic()
                os.write(terminal, b"z")
                expect("› " + "z" * length)
                samples.append((time.monotonic() - start) * 1000)
            os.write(terminal, b"\x01\x7f")
            samples.sort()
            p50, p95, p99 = (samples[(len(samples) * p + 99) // 100 - 1] for p in (50, 95, 99))
            print(f"50 key-to-PTY updates, P50/P95/P99: {p50:.2f}/{p95:.2f}/{p99:.2f} ms")
        os.write(terminal, b"Read fixture.txt\r")
        expect("Confirmed response 2")
        expect("Turn finished")
        expect("1 tools")
        assert FILE_TEXT not in screen_text(output), "Compact mode leaked tool output"
        os.write(terminal, b"\t")
        expect("fixture-read")
        os.write(terminal, b"\x0c")
        expect("Normal")
        os.write(terminal, b"\x0c")
        expect(FILE_TEXT)
        expect("Verbose")
        os.write(terminal, b"\x1b")
        # Let the terminal distinguish a standalone Escape from an escape sequence.
        expect("·  prompt")
        os.write(terminal, b"\x10")
        expect("Command palette")
        os.write(terminal, b"/route deepseek-official deepseek-v4-pro\r")
        expect("Session resumed")
        assert runtime not in process_table(), "SDK runtime survived route restart"
        runtime = next(pid for pid, parent in process_table().items() if parent == tui)
        os.write(terminal, b"Follow-up question\r")
        expect("Confirmed response 3")
        assert len(server.requests) == 3, len(server.requests)
        assert FILE_TEXT in json.dumps(server.requests[1]), "Provider never received real read output"
        assert "Read fixture.txt" in json.dumps(server.requests[2]), "Second turn lost session context"

        denied = work / "denied.txt"
        os.write(terminal, b"Test default denial\r")
        expect("Tool approval")
        expect("denied.txt.")
        assert not denied.exists(), "Tool ran before approval"
        os.write(terminal, b"\r")
        expect("Confirmed response 5")
        assert not denied.exists(), "Denied tool ran"

        allowed = work / "allowed.txt"
        os.write(terminal, b"Test allow once\r")
        expect("Tool approval")
        expect("allowed.txt.")
        assert not allowed.exists(), "Tool ran before approval"
        os.write(terminal, b"\t")
        expect("› Allow once")
        os.write(terminal, b"\r")
        expect("Confirmed response 7")
        assert allowed.read_text() == "approval-marker", "Approved tool did not run"

        os.write(terminal, b"Test question\r")
        expect("Agent question")
        expect("Pick fixture options")
        os.write(terminal, b" \t \tCustom fixture answer\r")
        expect("Confirmed response 9")
        result = next(item for message in server.requests[8]["messages"]
                      for item in message["content"] if isinstance(item, dict)
                      and item.get("tool_use_id") == "fixture-question")
        assert json.loads(result["content"][0]["text"]) == {"answers": [
            {"id": "fixture-choice", "selected": ["Alpha", "Beta"], "custom": "Custom fixture answer"}
        ]}

        os.write(terminal, b"Test child session\r")
        child_row = re.compile(r"Subagent · (\S+) · ok")
        expect(child_row)
        child_id = child_row.search(screen_text(output)).group(1)
        expect("Turn finished")
        os.write(terminal, b"\x10")
        expect("Command palette")
        os.write(terminal, f"/view {child_id}\r".encode())
        expect(CHILD_REPLY)
        os.write(terminal, b"\x10")
        expect("Command palette")
        os.write(terminal, b"/quit\r")
        expect("TTY_RESTORED", raw=True)
        _, status = os.waitpid(wrapper, 0)
        exited = True
        assert os.waitstatus_to_exitcode(status) == 0
        assert runtime not in process_table(), "SDK runtime was not reaped"
        print("Official SDK chat: multi-turn context, real read, fold/expand, route restart/resume, "
              "approvals, structured question, child session, terminal restore and process reap passed")
    finally:
        if not exited:
            try:
                os.killpg(wrapper, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(wrapper, 0)
        os.close(terminal)
        server.shutdown()
        thread.join()


def verify_ecosystem(binary, root, work, runtime):
    shared = root / ".dsh-tui/agent"
    home = root / "snapshot"
    home.mkdir()
    env = dict(os.environ, DSH_TUI_UPSTREAM=str(runtime), DSH_TUI_ROOT=str(root),
               DSH_TUI_DSH_HOME=str(shared), DEEPSEEK_API_KEY="local-fixture",
               DEEPSEEK_BASE_URL="http://127.0.0.1:9")
    adapter = (root / "runtime/host.mjs").resolve(strict=True)
    rows = json.loads(subprocess.check_output(
        ["node", adapter, "history", home, shared], env=env, timeout=15))
    row, = rows
    assert row["current"] and Path(row["cwd"]) == work.resolve()
    source, = (path for path in (shared / "sessions").rglob("session.v4.jsonl.zstd")
               if row["id"] in path.parts)
    source_before = source.read_bytes()
    journal = home / "history.ndjson"
    subprocess.run(["node", adapter, "stage", home, shared, row["id"], journal],
                   env=env, check=True, timeout=15)
    assert "Confirmed response 2" in journal.read_text()
    snapshot, = home.rglob("session.v4.jsonl.zstd")
    assert snapshot.read_bytes() == source_before == source.read_bytes()
    process, terminal = pty.fork()
    if process == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        os.chdir(work)
        os.execve(binary, [str(binary), "--history"], dict(env, TERM="xterm-256color"))
    output = bytearray()
    exited = False

    def expect(text, *, raw=False):
        def matches():
            return text.encode() in output if raw else text in screen_text(output)

        deadline = time.monotonic() + 15
        while not matches():
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([terminal], [], [], remaining)[0]:
                raise AssertionError(f"Ecosystem terminal did not show {text!r}")
            output.extend(os.read(terminal, 65536))

    try:
        expect("ready")
        expect(row["id"])
        os.write(terminal, b"\r")
        expect("Session resumed")
        expect("Test child session")
        inventory_path, = (root / ".dsh-tui").glob("ecosystem-*/lingxi-inventory.json")
        inventory = json.loads(inventory_path.read_text())
        assert any(provider["id"] == "deepseek-official" for provider in inventory["providers"])
        assert inventory["plugins"] and "skills" in inventory and inventory["mcp"] == []
        os.write(terminal, b"\x10/quit\r")
        expect("\x1b[?1049l", raw=True)
        _, status = os.waitpid(process, 0)
        exited = True
        assert os.waitstatus_to_exitcode(status) == 0
    finally:
        if not exited:
            try:
                os.killpg(process, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(process, 0)
        os.close(terminal)
    assert source.read_bytes() == source_before
    print("Official ecosystem: inventory and isolated current-format history snapshot passed")


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "target/debug/dsh-tui").resolve(strict=True)
    runtime = (ROOT / "runtime/upstream").resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="dsh-runtime-chat-") as tmp:
        tmp = Path(tmp).resolve()
        work = tmp / "workspace"
        work.mkdir()
        (work / "fixture.txt").write_text(FILE_TEXT + "\n", encoding="utf-8")
        root = tmp / "bundle"
        root.mkdir()
        (root / "runtime").mkdir()
        (root / "runtime/upstream").symlink_to(runtime, target_is_directory=True)
        (root / "runtime/upstream.lock.toml").symlink_to(ROOT / "runtime/upstream.lock.toml")
        (root / "runtime/host.mjs").symlink_to(ROOT / "runtime/host.mjs")
        with http.server.HTTPServer(("127.0.0.1", 0), Provider) as server:
            server.requests = []
            try:
                exercise(binary, root, work, server)
                verify_ecosystem(binary, root, work, runtime)
            except Exception:
                log = root / ".dsh-tui/runtime.log"
                if log.exists():
                    print(log.read_text(errors="replace")[-4000:], file=sys.stderr)
                raise


if __name__ == "__main__":
    main()
