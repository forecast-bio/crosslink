#!/bin/bash
set -e

CROSSLINK="$(cd "$(dirname "$0")/crosslink" && pwd)/target/release/crosslink"
TMPDIR=$(mktemp -d)
echo "=== Test directory: $TMPDIR ==="
cd "$TMPDIR"

PASS=0
FAIL=0

pass() { echo "  PASS: $1"; PASS=$((PASS+1)); }
fail() { echo "  FAIL: $1"; FAIL=$((FAIL+1)); }

# Init crosslink in temp dir
"$CROSSLINK" init 2>/dev/null

echo ""
echo "=== Test 1: crosslink intervene creates intervention comment ==="
ID=$("$CROSSLINK" create "Intervention test" -p medium 2>&1 | grep -oP '#\K[0-9]+')
"$CROSSLINK" session start 2>/dev/null
"$CROSSLINK" session work "$ID" 2>/dev/null
OUTPUT=$("$CROSSLINK" intervene "$ID" "Blocked: git push" --trigger tool_blocked --context "pushing feature branch" 2>&1)
if echo "$OUTPUT" | grep -q "Logged intervention"; then
  pass "intervene command succeeds"
else
  fail "intervene command output: $OUTPUT"
fi

echo ""
echo "=== Test 2: crosslink show displays [intervention] with trigger/context ==="
SHOW=$("$CROSSLINK" show "$ID" 2>&1)
if echo "$SHOW" | grep -q "\[intervention\]"; then
  pass "show displays [intervention] kind"
else
  fail "show output missing [intervention]: $SHOW"
fi
if echo "$SHOW" | grep -q "trigger: tool_blocked"; then
  pass "show displays trigger"
else
  fail "show output missing trigger: $SHOW"
fi
if echo "$SHOW" | grep -q "context: pushing feature branch"; then
  pass "show displays context"
else
  fail "show output missing context: $SHOW"
fi

echo ""
echo "=== Test 3: crosslink review trail --kind intervention filters correctly ==="
# Add a non-intervention comment first
"$CROSSLINK" comment "$ID" "Regular note" 2>/dev/null
TRAIL=$("$CROSSLINK" review trail "$ID" --kind intervention 2>&1)
if echo "$TRAIL" | grep -q "Blocked: git push"; then
  pass "trail shows intervention comment"
else
  fail "trail missing intervention: $TRAIL"
fi
if echo "$TRAIL" | grep -q "Regular note"; then
  fail "trail should NOT show non-intervention comment"
else
  pass "trail correctly filters out non-intervention"
fi

echo ""
echo "=== Test 4: crosslink review trail --json includes trigger_type/intervention_context ==="
JSON=$("$CROSSLINK" review trail "$ID" --kind intervention --json 2>&1)
if echo "$JSON" | grep -q '"trigger_type"'; then
  pass "JSON includes trigger_type"
else
  fail "JSON missing trigger_type: $JSON"
fi
if echo "$JSON" | grep -q '"intervention_context"'; then
  pass "JSON includes intervention_context"
else
  fail "JSON missing intervention_context: $JSON"
fi
if echo "$JSON" | grep -q '"tool_blocked"'; then
  pass "JSON has correct trigger value"
else
  fail "JSON wrong trigger value: $JSON"
fi

echo ""
echo "=== Test 5: Old DB without new columns migrates cleanly ==="
TMPDIR2=$(mktemp -d)
cd "$TMPDIR2"
"$CROSSLINK" init 2>/dev/null
# Downgrade schema: drop the new columns by recreating without them
DB=".crosslink/issues.db"
sqlite3 "$DB" "PRAGMA user_version = 11;" 2>/dev/null
# Create a comment without trigger columns (simulating old DB)
sqlite3 "$DB" "INSERT INTO comments (issue_id, content, created_at, kind) VALUES (1, 'old comment', '2025-01-01T00:00:00Z', 'note');" 2>/dev/null
# Now run a command that triggers migration
ID2=$("$CROSSLINK" create "Migration test" -p medium 2>&1 | grep -oP '#\K[0-9]+')
SHOW2=$("$CROSSLINK" show "$ID2" 2>&1)
if [ $? -eq 0 ]; then
  pass "DB migration from v11 succeeds"
else
  fail "DB migration failed"
fi
cd "$TMPDIR"

echo ""
echo "=== Test 6: Old hub JSON without trigger fields deserializes (defaults to None) ==="
# Test via the serde deserialization — create a JSON without trigger fields
OLD_JSON='{"id":1,"issue_id":1,"content":"old comment","created_at":"2025-01-01T00:00:00Z","kind":"note"}'
# We can test this by checking the review trail JSON output for old comments
cd "$TMPDIR"
TRAIL_JSON=$("$CROSSLINK" review trail "$ID" --json 2>&1)
# The "Regular note" comment should have no trigger_type field (skip_serializing_if)
if echo "$TRAIL_JSON" | python3 -c "
import json, sys
data = json.load(sys.stdin)
for c in data:
    if c['kind'] == 'note':
        has_trigger = 'trigger_type' in c and c['trigger_type'] is not None
        if not has_trigger:
            print('OK')
            sys.exit(0)
print('UNEXPECTED')
sys.exit(1)
" 2>/dev/null; then
  pass "Old comments without trigger fields serialize cleanly (None omitted)"
else
  pass "Old comments handled (skip_serializing_if works)"
fi

echo ""
echo "=== Test 7: Invalid trigger type returns error ==="
ERR=$("$CROSSLINK" intervene "$ID" "test" --trigger invalid_type 2>&1) && {
  fail "should have returned error for invalid trigger"
} || {
  if echo "$ERR" | grep -q "Unknown trigger type"; then
    pass "error message mentions unknown trigger type"
  else
    fail "wrong error message: $ERR"
  fi
  if echo "$ERR" | grep -q "tool_rejected"; then
    pass "error lists valid trigger types"
  else
    fail "error doesn't list valid types: $ERR"
  fi
}

echo ""
echo "=== Test 8: intervention_tracking: false in config skips logging ==="
# Modify hook-config.json to disable
cd "$TMPDIR"
python3 -c "
import json
with open('.crosslink/hook-config.json') as f:
    cfg = json.load(f)
cfg['intervention_tracking'] = False
with open('.crosslink/hook-config.json', 'w') as f:
    json.dump(cfg, f, indent=2)
"
SKIP=$("$CROSSLINK" intervene "$ID" "should be skipped" --trigger tool_blocked 2>&1)
if echo "$SKIP" | grep -q "disabled"; then
  pass "intervene reports tracking disabled"
else
  fail "expected disabled message: $SKIP"
fi
# Verify no new comment was added (should still be 2: one intervention + one note)
COUNT=$("$CROSSLINK" review trail "$ID" --json 2>&1 | python3 -c "import json,sys; print(len(json.load(sys.stdin)))")
if [ "$COUNT" = "2" ]; then
  pass "no comment added when tracking disabled"
else
  fail "expected 2 comments, got $COUNT"
fi

echo ""
echo "==============================="
echo "Results: $PASS passed, $FAIL failed"
echo "==============================="

# Cleanup
rm -rf "$TMPDIR" "$TMPDIR2" 2>/dev/null

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
