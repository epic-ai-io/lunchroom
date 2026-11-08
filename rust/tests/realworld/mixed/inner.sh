#!/bin/bash
# Proof 3, inside the container: the public Lunchroom 1.0.1 release program and the 1.1.0 program
# take turns writing one store (a user's ~/.lunchroom/store/state.json), each through its own
# `guard prompt` hook, fed the input an OpenClaw plugin gives it (hook.py).
#
# What it shows:
#   - which fields only 1.1.0 writes (read from the two programs' own stores, not assumed);
#   - what a 1.0.1 write does to them;
#   - the schema stamp, and `lunchroom doctor`'s "store was last written by an older Lunchroom
#     program" line;
#   - that a 1.1.0 write carries through a field it does not know (what a later program wrote).

set -u
. /opt/proof/lib.sh

R101=/usr/local/bin/lunchroom-1.0.1
R102=/usr/local/bin/lunchroom-1.1.0
STORE=$DEVHOME/.lunchroom/store/state.json
OLDLINE="Lunchroom's store was last written by an older Lunchroom program."

heading "facts of this system"
echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
measured "1.0.1: $($R101 --version 2>&1 | head -1)  sha256 $(sha256sum $R101 | cut -d' ' -f1)"
measured "1.1.0: $($R102 --version 2>&1 | head -1)  sha256 $(sha256sum $R102 | cut -d' ' -f1)"

make_user
mkdir -p "$DEVHOME/proof-plugin"
# The plugin file the hook input names; it names the program a plugin runs.
echo "const PROGRAM = \"$R102\";" > "$DEVHOME/proof-plugin/index.mjs"
chown -R dev "$DEVHOME/proof-plugin"
PLUGIN_ENV="PLUGIN_DIR=$DEVHOME/proof-plugin"

# hook_write PROGRAM SESSION: one OpenClaw-style prompt hook, run as dev by PROGRAM.
hook_write() {
    local program=$1 session=$2
    python3 /opt/proof/hook.py prompt "$session" "hello from $session" --typed > "$RUN/hook-$session.json"
    chmod 644 "$RUN/hook-$session.json"
    as_dev env "$PLUGIN_ENV" \
        sh -c "$program guard prompt < $RUN/hook-$session.json >/dev/null 2>&1"
}

# keys_of FILE: the top-level keys of a store, one per line.
keys_of() { python3 -c 'import json,sys; print("\n".join(sorted(json.load(open(sys.argv[1])))))' "$1"; }
# value_of FILE KEY: the JSON text of one top-level key, or ABSENT.
value_of() {
    python3 -c '
import json, sys
s = json.load(open(sys.argv[1]))
print(json.dumps(s[sys.argv[2]], sort_keys=True) if sys.argv[2] in s else "ABSENT")' "$1" "$2"
}

heading "1. which fields only 1.1.0 writes"
# A store written from scratch by 1.0.1, in a home of its own, gives 1.0.1's field set.
mkdir -p /home/dev/h101
chown dev /home/dev/h101
python3 /opt/proof/hook.py prompt B1 "baseline" --typed > "$RUN/hook-B1.json"
chmod 644 "$RUN/hook-B1.json"
as_dev env HOME=/home/dev/h101 sh -c "$R101 guard prompt < $RUN/hook-B1.json >/dev/null 2>&1"
BASE101=/home/dev/h101/.lunchroom/store/state.json
[ -f "$BASE101" ] && pass "1.0.1 wrote a store of its own at $BASE101" || { fail "1.0.1 wrote no store"; finish; }
hook_write $R102 S1
[ -f "$STORE" ] && pass "1.1.0 wrote the store $STORE" || { fail "1.1.0 wrote no store"; finish; }
keys_of "$BASE101" > "$RUN/keys101"
keys_of "$STORE" > "$RUN/keys102"
ONLY102=$(comm -13 "$RUN/keys101" "$RUN/keys102" | tr '\n' ' ')
ONLY101=$(comm -23 "$RUN/keys101" "$RUN/keys102" | tr '\n' ' ')
measured "fields only 1.1.0 writes: $ONLY102"
measured "fields only 1.0.1 writes: ${ONLY101:-none}"
[ -n "$ONLY102" ] && pass "1.1.0 writes fields 1.0.1 does not: $ONLY102" || fail "1.1.0 writes no field 1.0.1 lacks"
for k in $ONLY102; do measured "1.1.0's store: $k = $(value_of "$STORE" "$k" | head -c 160)"; done
[ "$(value_of "$STORE" schema)" = "2" ] && pass "the 1.1.0 store carries the layout number: schema = 2" \
    || fail "the 1.1.0 store's schema is $(value_of "$STORE" schema), not 2"
LGF=$(value_of "$STORE" latest_guard_fingerprint)
[ "$LGF" != "ABSENT" ] && [ "$LGF" != "{}" ] \
    && pass "latest_guard_fingerprint holds data in the 1.1.0 store: $(echo "$LGF" | head -c 100)" \
    || fail "latest_guard_fingerprint is empty in the 1.1.0 store: $LGF (the test would prove nothing)"

heading "2. doctor, while 1.1.0 wrote the store last"
doctor_text() {
    rm -f "$RUN/doctor.log"
    pty doctor "${DEV[@]}" "$R102" doctor
    python3 /opt/proof/plaintext.py "$RUN/doctor.log"
}
doctor_text > "$RUN/doctor1.out"
[ -s "$RUN/doctor1.out" ] && pass "doctor ran (it printed $(wc -l < "$RUN/doctor1.out") lines)" || fail "doctor printed nothing"
grep -qF "$OLDLINE" "$RUN/doctor1.out" && fail "doctor says the store was written by an older program, but 1.1.0 wrote it" \
    || pass "doctor does not say the store was last written by an older program (control)"

heading "3. a 1.0.1 write into the 1.1.0 store"
cp "$STORE" "$RUN/store-after-102.json"
hook_write $R101 S2
S2IN=$(python3 -c 'import json; s=json.load(open("'"$STORE"'")); print("yes" if any("S2" in k for k in s.get("seen", [])) else "no")')
[ "$S2IN" = yes ] && pass "1.0.1 wrote into the store: it records the session S2" \
    || fail "1.0.1 did not record S2: the write did not happen"
for k in $ONLY102; do
    before=$(value_of "$RUN/store-after-102.json" "$k")
    after=$(value_of "$STORE" "$k")
    measured "$k: after the 1.1.0 write = $(echo "$before" | head -c 100); after the 1.0.1 write = $(echo "$after" | head -c 100)"
done
LOST=""
for k in $ONLY102; do
    before=$(value_of "$RUN/store-after-102.json" "$k")
    after=$(value_of "$STORE" "$k")
    # A field with no data to lose: absent, empty, 0.
    case "$before" in ABSENT|'[]'|'{}'|0|null|'""') continue ;; esac
    [ "$after" = "$before" ] || LOST="$LOST $k"
done
# docs/spec/lunchroom-spec.md:891 (section 15, limit 26): a 1.0.1 program "rewrites the store
# without the 1.1.0 fields and without the layout number". The spec governs: the fields are dropped.
if [ -n "$LOST" ]; then
    pass "a 1.0.1 write drops the 1.1.0-only fields that held data (spec section 15 limit 26):$LOST"
else
    fail "a 1.0.1 write kept every 1.1.0-only field that held data; spec section 15 limit 26 says it drops them"
fi
[ "$(value_of "$STORE" schema)" = "ABSENT" ] && pass "the 1.0.1 write left no schema stamp (1.0.1 knows none): the stamp reads as 0, 'written by an older program'" \
    || fail "the store still has a schema stamp after the 1.0.1 write: $(value_of "$STORE" schema)"

heading "4. doctor, after the 1.0.1 write"
doctor_text > "$RUN/doctor2.out"
sed 's/^/    /' "$RUN/doctor2.out"
grep -qF "$OLDLINE" "$RUN/doctor2.out" \
    && pass "doctor says: $OLDLINE" \
    || fail "doctor does not say the store was last written by an older Lunchroom program"
grep -F "$OLDLINE" "$RUN/doctor2.out" | grep -qF "Start those sessions anew." \
    && pass "the line tells the user to start those sessions anew" || fail "the line does not tell the user to start sessions anew"
grep -F "$OLDLINE" "$RUN/doctor2.out" | grep -q "^FAIL" \
    && pass "the line is a FAIL line" || fail "the line is not a FAIL line"

heading "5. the next 1.1.0 write restores the stamp, not the lost data"
hook_write $R102 S3
[ "$(value_of "$STORE" schema)" = "2" ] && pass "after a 1.1.0 write the stamp is schema = 2 again" \
    || fail "after a 1.1.0 write the schema is $(value_of "$STORE" schema)"
doctor_text > "$RUN/doctor3.out"
grep -qF "$OLDLINE" "$RUN/doctor3.out" && fail "doctor still says an older program wrote last" \
    || pass "doctor no longer says an older program wrote last"
measured "latest_guard_fingerprint after the next 1.1.0 write: $(value_of "$STORE" latest_guard_fingerprint | head -c 100)"

heading "6. a field neither program knows (what a later program wrote)"
python3 - "$STORE" <<'PY'
import json, sys
p = sys.argv[1]
s = json.load(open(p))
s["written_by_a_later_program"] = {"x": 1}
json.dump(s, open(p, "w"))
PY
[ "$(value_of "$STORE" written_by_a_later_program)" = '{"x": 1}' ] && pass "the unknown field is in the store" || fail "the unknown field was not put in the store"
hook_write $R102 S4
[ "$(value_of "$STORE" written_by_a_later_program)" = '{"x": 1}' ] \
    && pass "a 1.1.0 write carries the unknown field through" \
    || fail "a 1.1.0 write lost the unknown field: $(value_of "$STORE" written_by_a_later_program)"
hook_write $R101 S5
measured "after a 1.0.1 write the unknown field is: $(value_of "$STORE" written_by_a_later_program)"

finish
