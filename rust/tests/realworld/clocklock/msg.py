#!/usr/bin/env python3
"""Reads, and for the clock proof plants a stamp in, one message of Lunchroom's store.

    msg.py STATE ID                       print the message's status, failed_attempts, presented_at, handed_over
    msg.py STATE ID plant REL HANDED      first set failed_attempts to 0, presented_at to now+REL seconds
                                          (REL may be negative) and handed_over to HANDED (0 or 1)

A rootless container cannot set the wall clock (no CAP_SYS_TIME, and the host's clock is not to be
touched) and Lunchroom is a static program, so no faketime can reach it. A stamp that a clock which
stepped back would have left is planted in the store instead; the program then judges it on the
real clock.
"""

import json
import sys
import time

state, mid = sys.argv[1], sys.argv[2]
with open(state) as f:
    store = json.load(f)
message = store["messages"][mid]
if len(sys.argv) > 3 and sys.argv[3] == "plant":
    rel, handed = int(sys.argv[4]), sys.argv[5] == "1"
    message["failed_attempts"] = 0
    message["presented_at"] = int(time.time()) + rel
    message["handed_over"] = handed
    with open(state, "w") as f:
        json.dump(store, f)
print(json.dumps({k: message.get(k) for k in ("status", "failed_attempts", "presented_at", "handed_over")}, sort_keys=True))
