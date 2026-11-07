#!/usr/bin/env python3
"""A server that accepts connections on 127.0.0.1:8099 and never answers.

The proofs point Claude Code's ANTHROPIC_BASE_URL here, so a session that has sent a prompt waits
for a reply that never comes: the real `claude` program stays a long-lived, attended, interactive
session (its hooks ran when the prompt was submitted), with no network and no account.
"""

import socket

server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", 8099))
server.listen(50)
held = []
while True:
    connection, _ = server.accept()
    held.append(connection)
