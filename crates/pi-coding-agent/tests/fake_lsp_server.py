#!/usr/bin/env python3
"""Tiny fake LSP server used by pi-rs's transport tests.

Speaks just enough JSON-RPC over stdio to exercise:

* the `initialize` handshake (replies with a fixed `serverInfo.name`);
* concurrent request id correlation (`test/echo` honours an optional
  `delay_ms` so responses can be reordered relative to send order);
* RPC errors (`test/error` synthesises the `{code, message}` the caller
  asks for);
* server-originated notifications (`test/push_notification` triggers a
  `window/logMessage`).

Deliberately written without `asyncio` or third-party deps so it works
on any python3 install. Concurrency is achieved with a worker thread
pool; output is serialised behind a `threading.Lock` so frames don't
interleave on stdout.
"""

import json
import os
import sys
import threading
import time

OUT_LOCK = threading.Lock()


def write_frame(obj):
    body = json.dumps(obj).encode("utf-8")
    header = f"Content-Length: {len(body)}\r\n\r\n".encode("ascii")
    with OUT_LOCK:
        sys.stdout.buffer.write(header)
        sys.stdout.buffer.write(body)
        sys.stdout.buffer.flush()


def read_frame():
    """Read one Content-Length-framed message off stdin. Returns None at EOF."""
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        text = line.decode("ascii", errors="replace").rstrip("\r\n")
        if ":" in text:
            k, v = text.split(":", 1)
            headers[k.strip().lower()] = v.strip()
    length = int(headers["content-length"])
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode("utf-8"))


def handle_message(msg):
    method = msg.get("method")
    params = msg.get("params") or {}
    msg_id = msg.get("id")

    if method == "initialize":
        write_frame(
            {
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {
                    "capabilities": {"textDocumentSync": 1},
                    "serverInfo": {"name": "fake-lsp-server", "version": "0.0.0"},
                },
            }
        )
    elif method == "initialized":
        # notification, no response
        return
    elif method == "test/echo":
        delay = float(params.get("delay_ms", 0)) / 1000.0
        if delay > 0:
            time.sleep(delay)
        write_frame({"jsonrpc": "2.0", "id": msg_id, "result": params})
    elif method == "test/error":
        write_frame(
            {
                "jsonrpc": "2.0",
                "id": msg_id,
                "error": {
                    "code": int(params.get("code", -32000)),
                    "message": params.get("message", ""),
                },
            }
        )
    elif method == "test/push_notification":
        # Fire a server→client notification. No response to the trigger
        # itself (it's a notification, no id).
        write_frame(
            {
                "jsonrpc": "2.0",
                "method": params.get("method", "window/logMessage"),
                "params": params.get("params", {}),
            }
        )
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": msg_id, "result": None})
    elif method == "exit":
        os._exit(0)
    elif method == "textDocument/typeDefinition":
        # Return a single Location pointing at a synthetic type def.
        write_frame(
            {
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {
                    "uri": params["textDocument"]["uri"],
                    "range": {
                        "start": {"line": 10, "character": 0},
                        "end": {"line": 10, "character": 5},
                    },
                    "_marker": "type_definition",
                },
            }
        )
    elif method == "textDocument/implementation":
        # Return an array of Locations.
        write_frame(
            {
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": [
                    {
                        "uri": params["textDocument"]["uri"],
                        "range": {
                            "start": {"line": 20, "character": 0},
                            "end": {"line": 20, "character": 8},
                        },
                        "_marker": "implementation",
                    }
                ],
            }
        )
    elif method == "textDocument/rename":
        # Echo the requested newName back inside a synthetic
        # WorkspaceEdit so tests can assert on it.
        uri = params["textDocument"]["uri"]
        new_name = params.get("newName", "")
        write_frame(
            {
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {
                    "changes": {
                        uri: [
                            {
                                "range": {
                                    "start": {"line": 0, "character": 0},
                                    "end": {"line": 0, "character": 3},
                                },
                                "newText": new_name,
                            }
                        ]
                    },
                    "_marker": "rename",
                },
            }
        )
    elif method == "textDocument/codeAction":
        # Echo the range back inside a single canned CodeAction.
        write_frame(
            {
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": [
                    {
                        "title": "fake quickfix",
                        "kind": "quickfix",
                        "_marker": "code_actions",
                        "_echo_range": params.get("range"),
                    }
                ],
            }
        )
    else:
        if msg_id is not None:
            write_frame(
                {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "error": {"code": -32601, "message": f"Method not found: {method}"},
                }
            )


def main():
    while True:
        msg = read_frame()
        if msg is None:
            return
        # Spawn a thread per message so `delay_ms` doesn't block other
        # requests — that's what the id-correlation test relies on.
        t = threading.Thread(target=handle_message, args=(msg,), daemon=True)
        t.start()


if __name__ == "__main__":
    main()
