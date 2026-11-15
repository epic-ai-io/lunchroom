#!/bin/bash
# Proof 3, inside the container: the public Lunchroom 1.0.1 release program and the 1.1 program take
# turns writing one store (a user's ~/.lunchroom/store, the user-mode store neither program is
# setgid for here), each through its own `guard prompt` hook, fed the input an OpenClaw plugin gives
# it (hook.py).
#
# 1.1 (spec section 10 D1.2, docs/spec/lunchroom-spec.md): "The store file carries the number of the
# program layout that wrote it (2 in 1.1), and every program keeps each field it does not know through
# every read and write. The fields 1.1 adds ... are kept in a file of their own beside the store file,
# read and written under the same `state.flock` lock, which a 1.0.1 program takes and never rewrites
# the second file under, so a 1.0.1 program still running loses none of them." And its test: "a 1.0.1
# program's write of the store leaves 1.1's own fields as they were" (section 15 limit 26: it no
# longer loses 1.1's fields). In 1.0.2 the 1.0.1 write DROPPED those fields; this proof now expects
# them kept.
#
# What it shows:
#   - which fields 1.1 keeps in the second file, state.side.json (read from the two programs' own
#     files, not assumed);
#   - that a 1.0.1 write leaves the second file byte for byte as it was, and that the 1.1 fields are
#     still readable after it;
#   - the schema stamp, and what `lunchroom doctor` says of the last writer;
#   - that a 1.1 write carries through a field it does not know (what a later program wrote).

set -u
. /opt/proof/lib.sh

R101=/usr/local/bin/lunchroom-1.0.1
R11=/usr/local/bin/lunchroom-1.1
STOREDIR=$DEVHOME/.lunchroom/store
STORE=$STOREDIR/state.json
SIDE=$STOREDIR/state.side.json
OLDLINE="Lunchroom's store was last written by an older Lunchroom program."

heading "facts of this system"
echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
measured "1.0.1: $($R101 --version 2>&1 | head -1)  sha256 $(sha256sum $R101 | cut -d' ' -f1)"
measured "1.1: $($R11 --version 2>&1 | head -1)  sha256 $(sha256sum $R11 | cut -d' ' -f1)"

make_user
mkdir -p "$DEVHOME/proof-plugin"
# The plugin file the hook input names; it names the program a plugin runs.
echo "const PROGRAM = \"$R11\";" > "$DEVHOME/proof-plugin/index.mjs"
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

# keys_of FILE: the top-level keys of a store file, one per line.
keys_of() { python3 -c 'import json,sys; print("\n".join(sorted(json.load(open(sys.argv[1])))))' "$1"; }
# value_of FILE KEY: the JSON text of one top-level key, or ABSENT.
value_of() {
    python3 -c '
import json, sys
s = json.load(open(sys.argv[1]))
print(json.dumps(s[sys.argv[2]], sort_keys=True) if sys.argv[2] in s else "ABSENT")' "$1" "$2"
}

heading "1. which fields 1.1 keeps in a file of its own (D1.2)"
# A store written from scratch by 1.0.1, in a home of its own, gives 1.0.1's field set.
mkdir -p /home/dev/h101
chown dev /home/dev/h101
python3 /opt/proof/hook.py prompt B1 "baseline" --typed > "$RUN/hook-B1.json"
chmod 644 "$RUN/hook-B1.json"
as_dev env HOME=/home/dev/h101 sh -c "$R101 guard prompt < $RUN/hook-B1.json >/dev/null 2>&1"
BASE101=/home/dev/h101/.lunchroom/store/state.json
[ -f "$BASE101" ] && pass "1.0.1 wrote a store of its own at $BASE101" || { fail "1.0.1 wrote no store"; finish; }
[ -f /home/dev/h101/.lunchroom/store/state.side.json ] && fail "1.0.1 wrote a second file" || pass "1.0.1 writes no second file"
hook_write $R11 S1
[ -f "$STORE" ] && pass "1.1 wrote the store $STORE" || { fail "1.1 wrote no store"; finish; }
keys_of "$BASE101" > "$RUN/keys101"
keys_of "$STORE" > "$RUN/keys11"
ONLY11=$(comm -13 "$RUN/keys101" "$RUN/keys11" | tr '\n' ' ')
ONLY101=$(comm -23 "$RUN/keys101" "$RUN/keys11" | tr '\n' ' ')
measured "fields in 1.1's state.json that 1.0.1's does not have: ${ONLY11:-none}"
measured "fields in 1.0.1's state.json that 1.1's does not have: ${ONLY101:-none}"
[ -f "$SIDE" ] && pass "1.1 keeps the fields it adds in a file of its own beside the store file: $SIDE" \
    || fail "1.1 wrote no second file $SIDE (D1.2)"
SIDEKEYS=$([ -f "$SIDE" ] && keys_of "$SIDE" | tr '\n' ' ')
measured "the fields in the second file: ${SIDEKEYS:-none}"
[ -n "$SIDEKEYS" ] && pass "the second file holds fields: $SIDEKEYS" || fail "the second file holds no field"
for k in $SIDEKEYS; do measured "1.1's second file: $k = $(value_of "$SIDE" "$k" | head -c 160)"; done
# 2026-11-14: the second file's layout (lunchroom-core mail/side.rs): `top` holds each top-level field
# 1.0.1 has no name for; `sessions` and the other containers hold, per entry of a map or list 1.0.1
# has, the fields 1.0.1 does not know. So a 1.0.1 top-level field must never be inside `top` (the
# earlier check read the container names themselves as fields).
top_keys() { python3 -c '
import json, sys
s = json.load(open(sys.argv[1]))
print("\n".join(sorted((s.get("top") or {}).keys())))' "$1"; }
TOPKEYS=$([ -f "$SIDE" ] && top_keys "$SIDE" | tr '\n' ' ')
measured "the top-level fields kept in the second file (top): ${TOPKEYS:-none}"
for k in $TOPKEYS; do
    grep -qx "$k" "$RUN/keys101" && fail "the second file's top holds $k, a field 1.0.1 writes in the store file"
done
[ "$(value_of "$STORE" schema)" = "2" ] && pass "the store file carries the layout number: schema = 2" \
    || fail "the 1.1 store's schema is $(value_of "$STORE" schema), not 2"
top_value() { python3 -c '
import json, sys
t = (json.load(open(sys.argv[1])).get("top") or {})
print(json.dumps(t[sys.argv[2]], sort_keys=True) if sys.argv[2] in t else "ABSENT")' "$1" "$2"; }
LGF=$([ -f "$SIDE" ] && top_value "$SIDE" latest_guard_fingerprint || echo ABSENT)
[ "$LGF" != "ABSENT" ] && [ "$LGF" != "{}" ] \
    && pass "latest_guard_fingerprint holds data in the second file: $(echo "$LGF" | head -c 100)" \
    || fail "latest_guard_fingerprint is not in the second file or is empty: $LGF (the test would prove nothing)"

heading "2. doctor, while 1.1 wrote the store last"
doctor_text() {
    rm -f "$RUN/doctor.log"
    pty doctor "${DEV[@]}" "$R11" doctor
    python3 /opt/proof/plaintext.py "$RUN/doctor.log"
}
doctor_text > "$RUN/doctor1.out"
[ -s "$RUN/doctor1.out" ] && pass "doctor ran (it printed $(wc -l < "$RUN/doctor1.out") lines)" || fail "doctor printed nothing"
grep -qF "$OLDLINE" "$RUN/doctor1.out" && fail "doctor says the store was written by an older program, but 1.1 wrote it" \
    || pass "doctor does not say the store was last written by an older program (control)"

heading "3. a 1.0.1 write into the 1.1 store: 1.1's own fields are left as they were (D1.2)"
cp "$STORE" "$RUN/store-after-11.json"
cp "$SIDE" "$RUN/side-after-11.json"
SIDE_SUM=$(sha256sum "$SIDE" | cut -d' ' -f1)
hook_write $R101 S2
S2IN=$(python3 -c 'import json; s=json.load(open("'"$STORE"'")); print("yes" if any("S2" in k for k in s.get("seen", [])) else "no")')
[ "$S2IN" = yes ] && pass "1.0.1 wrote into the store: it records the session S2" \
    || fail "1.0.1 did not record S2: the write did not happen"
[ -f "$SIDE" ] && [ "$(sha256sum "$SIDE" | cut -d' ' -f1)" = "$SIDE_SUM" ] \
    && pass "a 1.0.1 write left the second file byte for byte as it was (D1.2, limit 26)" \
    || fail "the second file changed or is gone after a 1.0.1 write"
for k in $SIDEKEYS; do
    [ "$(value_of "$RUN/side-after-11.json" "$k")" = "$(value_of "$SIDE" "$k")" ] \
        && pass "1.1's field $k is as it was after the 1.0.1 write" \
        || fail "1.1's field $k changed after the 1.0.1 write: $(value_of "$SIDE" "$k" | head -c 100)"
done
measured "schema after the 1.0.1 write: $(value_of "$STORE" schema)"
measured "the store file's fields after the 1.0.1 write: $(keys_of "$STORE" | tr '\n' ' ')"

heading "4. doctor, after the 1.0.1 write"
doctor_text > "$RUN/doctor2.out"
sed 's/^/    /' "$RUN/doctor2.out"
if grep -qF "$OLDLINE" "$RUN/doctor2.out"; then
    measured "doctor says: $OLDLINE"
    grep -F "$OLDLINE" "$RUN/doctor2.out" | grep -qF "Start those sessions anew." \
        && pass "the line tells the user to start those sessions anew" || fail "the line does not tell the user to start sessions anew"
else
    measured "doctor does not say the store was last written by an older program: 1.1 lost nothing to that write, and the spec states no doctor line for it"
fi

heading "5. the next 1.1 write restores the stamp"
hook_write $R11 S3
[ "$(value_of "$STORE" schema)" = "2" ] && pass "after a 1.1 write the stamp is schema = 2 again" \
    || fail "after a 1.1 write the schema is $(value_of "$STORE" schema)"
doctor_text > "$RUN/doctor3.out"
grep -qF "$OLDLINE" "$RUN/doctor3.out" && fail "doctor still says an older program wrote last" \
    || pass "doctor no longer says an older program wrote last"
for k in $SIDEKEYS; do
    [ "$(value_of "$SIDE" "$k")" != "ABSENT" ] && pass "1.1's field $k is still in the second file after the next 1.1 write" \
        || fail "1.1's field $k is gone after the next 1.1 write"
done

heading "6. a field neither program knows (what a later program wrote)"
python3 - "$STORE" <<'PY'
import json, sys
p = sys.argv[1]
s = json.load(open(p))
s["written_by_a_later_program"] = {"x": 1}
json.dump(s, open(p, "w"))
PY
[ "$(value_of "$STORE" written_by_a_later_program)" = '{"x": 1}' ] && pass "the unknown field is in the store" || fail "the unknown field was not put in the store"
hook_write $R11 S4
# A 1.1 write keeps the field in the second file's `top` (mail/side.rs: every top-level field 1.0.1
# has no name for is taken out of the store file at a write and put back at a read).
KEPT=$(value_of "$STORE" written_by_a_later_program)
[ "$KEPT" = ABSENT ] && [ -f "$SIDE" ] && KEPT=$(top_value "$SIDE" written_by_a_later_program)
[ "$KEPT" = '{"x": 1}' ] \
    && pass "a 1.1 write carries the unknown field through, in the store file or the second file's top (D1.2)" \
    || fail "a 1.1 write lost the unknown field: $KEPT"
hook_write $R101 S5
measured "after a 1.0.1 write the unknown field is: $(value_of "$STORE" written_by_a_later_program)"

finish
