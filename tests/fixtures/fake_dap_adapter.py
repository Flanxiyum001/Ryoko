#!/usr/bin/env python3
"""
Deterministic Fake DAP Adapter for Ryoko automated and manual testing.
Implements minimal Debug Adapter Protocol over stdio with Content-Length framing.
"""

import json
import sys

def send_message(payload: dict) -> None:
    data = json.dumps(payload, separators=(',', ':')).encode('utf-8')
    header = f"Content-Length: {len(data)}\r\n\r\n".encode('ascii')
    sys.stdout.buffer.write(header + data)
    sys.stdout.buffer.flush()

def read_message() -> dict | None:
    content_length = None
    header_line = b""
    while True:
        byte = sys.stdin.buffer.read(1)
        if not byte:
            return None
        header_line += byte
        if header_line.endswith(b"\r\n\r\n"):
            # Header block complete
            for line in header_line.decode('ascii', errors='replace').split("\r\n"):
                if line.lower().startswith("content-length:"):
                    content_length = int(line.split(":", 1)[1].strip())
            break
        elif len(header_line) > 8192:
            return None

    if content_length is None:
        return None

    body_bytes = sys.stdin.buffer.read(content_length)
    if len(body_bytes) < content_length:
        return None
    try:
        return json.loads(body_bytes.decode('utf-8'))
    except Exception:
        return None

def main() -> None:
    seq = 0
    breakpoints_store = {}
    launch_request = None

    def next_seq() -> int:
        nonlocal seq
        seq += 1
        return seq

    while True:
        msg = read_message()
        if msg is None:
            break
        if msg.get("type") != "request":
            continue

        cmd = msg.get("command")
        req_seq = msg.get("seq", 0)
        args = msg.get("arguments", {})

        if cmd == "initialize":
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": "initialize",
                "success": True,
                "body": {
                    "supportsConfigurationDoneRequest": True,
                    "supportsFunctionBreakpoints": False,
                    "supportsConditionalBreakpoints": False,
                }
            })
        elif cmd == "launch":
            # Match adapters that wait for launch before accepting configuration
            # and only acknowledge launch after configurationDone.
            launch_request = req_seq
            send_message({
                "seq": next_seq(),
                "type": "event",
                "event": "initialized",
            })

        elif cmd == "setBreakpoints":
            source = args.get("source", {})
            src_path = source.get("path", "unknown")
            bps = args.get("breakpoints", [])
            verified_bps = []
            for i, bp in enumerate(bps):
                line = bp.get("line", 1)
                verified_bps.append({
                    "id": i + 1,
                    "verified": True,
                    "line": line,
                    "source": source,
                })
            breakpoints_store[src_path] = verified_bps
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": "setBreakpoints",
                "success": True,
                "body": {
                    "breakpoints": verified_bps,
                }
            })

        elif cmd == "configurationDone":
            if launch_request is None:
                send_message({
                    "seq": next_seq(),
                    "type": "response",
                    "request_seq": req_seq,
                    "command": cmd,
                    "success": False,
                    "message": "launch must precede configuration",
                })
                continue
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": launch_request,
                "command": "launch",
                "success": True,
            })
            launch_request = None
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": "configurationDone",
                "success": True,
            })
            # Once configured, simulate hitting a breakpoint
            send_message({
                "seq": next_seq(),
                "type": "event",
                "event": "stopped",
                "body": {
                    "reason": "breakpoint",
                    "description": "Paused at breakpoint",
                    "threadId": 1,
                    "allThreadsStopped": True,
                }
            })

        elif cmd == "threads":
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": "threads",
                "success": True,
                "body": {
                    "threads": [
                        {"id": 1, "name": "main"}
                    ]
                }
            })

        elif cmd == "stackTrace":
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": "stackTrace",
                "success": True,
                "body": {
                    "stackFrames": [
                        {
                            "id": 10,
                            "name": "calculate_total()",
                            "line": 42,
                            "column": 5,
                            "source": {
                                "name": "order.rs",
                                "path": "/workspace/src/order.rs"
                            }
                        },
                        {
                            "id": 11,
                            "name": "process_order()",
                            "line": 108,
                            "column": 12,
                            "source": {
                                "name": "order.rs",
                                "path": "/workspace/src/order.rs"
                            }
                        },
                        {
                            "id": 12,
                            "name": "main()",
                            "line": 15,
                            "column": 1,
                            "source": {
                                "name": "main.rs",
                                "path": "/workspace/src/main.rs"
                            }
                        }
                    ],
                    "totalFrames": 3
                }
            })

        elif cmd == "scopes":
            frame_id = args.get("frameId", 0)
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": "scopes",
                "success": True,
                "body": {
                    "scopes": [
                        {
                            "name": "Locals",
                            "variablesReference": 1000 + frame_id,
                            "expensive": False,
                        },
                        {
                            "name": "Globals",
                            "variablesReference": 2000 + frame_id,
                            "expensive": True,
                        }
                    ]
                }
            })

        elif cmd == "variables":
            var_ref = args.get("variablesReference", 0)
            if var_ref == 1010: # Locals for frame 10
                vars_list = [
                    {"name": "unit_price", "value": "19.99", "type": "f64", "variablesReference": 0},
                    {"name": "quantity", "value": "3", "type": "u32", "variablesReference": 0},
                    {"name": "tax_rate", "value": "0.08", "type": "f64", "variablesReference": 0},
                    {"name": "customer", "value": "Customer { id: 402, name: \"Alice\" }", "type": "Customer", "variablesReference": 5001},
                ]
            elif var_ref == 5001: # Customer details
                vars_list = [
                    {"name": "id", "value": "402", "type": "u64", "variablesReference": 0},
                    {"name": "name", "value": "\"Alice\"", "type": "String", "variablesReference": 0},
                ]
            elif var_ref == 1011: # Locals for frame 11
                vars_list = [
                    {"name": "order_id", "value": "9001", "type": "u64", "variablesReference": 0},
                    {"name": "status", "value": "\"Pending\"", "type": "String", "variablesReference": 0},
                ]
            else:
                vars_list = [
                    {"name": "ENV", "value": "\"development\"", "type": "String", "variablesReference": 0}
                ]

            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": "variables",
                "success": True,
                "body": {
                    "variables": vars_list
                }
            })

        elif cmd in ("next", "stepIn", "stepOut"):
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": cmd,
                "success": True,
            })
            send_message({
                "seq": next_seq(),
                "type": "event",
                "event": "stopped",
                "body": {
                    "reason": "step",
                    "description": f"Step ({cmd}) completed",
                    "threadId": 1,
                    "allThreadsStopped": True,
                }
            })

        elif cmd == "continue":
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": "continue",
                "success": True,
            })
            send_message({
                "seq": next_seq(),
                "type": "event",
                "event": "continued",
                "body": {
                    "threadId": 1,
                    "allThreadsContinued": True,
                }
            })
            # Simulate completion after continue
            send_message({
                "seq": next_seq(),
                "type": "event",
                "event": "terminated",
            })
            send_message({
                "seq": next_seq(),
                "type": "event",
                "event": "exited",
                "body": {"exitCode": 0}
            })
            break

        elif cmd == "disconnect":
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": "disconnect",
                "success": True,
            })
            break

        else:
            send_message({
                "seq": next_seq(),
                "type": "response",
                "request_seq": req_seq,
                "command": cmd,
                "success": False,
                "message": f"unrecognized command: {cmd}",
            })

if __name__ == "__main__":
    main()
