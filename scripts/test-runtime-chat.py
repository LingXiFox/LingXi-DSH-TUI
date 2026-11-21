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
            return text.encode() in output if raw else text in screen_text(output)

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
        expect("Commands")
        os.write(terminal, b"/route deepseek-official deepseek-v4-pro\r")
        expect("Session resumed")
        assert runtime not in process_table(), "SDK runtime survived route restart"
        runtime = next(pid for pid, parent in process_table().items() if parent == tui)
        os.write(terminal, b"Follow-up question\r")
        expect("Confirmed response 3")
        assert len(server.requests) == 3, len(server.requests)
        assert FILE_TEXT in json.dumps(server.requests[1]), "Provider never received real read output"
        assert "Read fixture.txt" in json.dumps(server.requests[2]), "Second turn lost session context"
        os.write(terminal, b"\x10")
        expect("Commands")
        os.write(terminal, b"/quit\r")
        expect("TTY_RESTORED", raw=True)
        _, status = os.waitpid(wrapper, 0)
        exited = True
        assert os.waitstatus_to_exitcode(status) == 0
        assert runtime not in process_table(), "SDK runtime was not reaped"
        print("Official SDK chat: two turns, real read, fold/expand, route restart/resume, terminal restore and process reap passed")
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


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "target/debug/dsh-tui").resolve(strict=True)
    runtime = (ROOT / "runtime/upstream").resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="dsh-runtime-chat-") as tmp:
        work = Path(tmp) / "workspace"
        work.mkdir()
        (work / "fixture.txt").write_text(FILE_TEXT + "\n", encoding="utf-8")
        root = Path(tmp) / "bundle"
        root.mkdir()
        (root / "runtime").symlink_to(runtime, target_is_directory=True)
        (root / "runtime.lock.toml").symlink_to(ROOT / "runtime/upstream.lock.toml")
        with http.server.HTTPServer(("127.0.0.1", 0), Provider) as server:
            server.requests = []
            try:
                exercise(binary, root, work, server)
            except Exception:
                log = root / ".dsh-tui/runtime.log"
                if log.exists():
                    print(log.read_text(errors="replace")[-4000:], file=sys.stderr)
                raise


if __name__ == "__main__":
    main()
