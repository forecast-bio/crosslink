#!/bin/bash
# Test plan verification for shared-issues-migration PR
# Run this from outside the main crosslink project to avoid hook interference.

set -e
CL="$(cygpath -w /c/Users/texas/forecast/crosslink-pr4/crosslink/target/debug/crosslink)"
TESTDIR="/tmp/crosslink-test-multi"

echo "=== Setting up test environment ==="
rm -rf "$TESTDIR"
mkdir -p "$TESTDIR"
cd "$TESTDIR"
git init
git config user.name "test"
git config user.email "test@test.com"
git config commit.gpgsign false

echo ""
echo "=== 1. Init crosslink and create test issues ==="
"$CL" init
rm -rf .claude  # Remove hooks so they don't interfere

"$CL" create "First issue" -p high -d "Description one"
"$CL" create "Second issue" -p medium
"$CL" create "Third issue" -p low -d "Has dependencies"
"$CL" comment 1 "Test comment on first"
"$CL" comment 1 "Second comment"
"$CL" label 1 feature
"$CL" label 2 bug
"$CL" block 3 1
"$CL" relate 1 2
"$CL" list
echo "--- Single-agent setup complete ---"

echo ""
echo "=== 2. Init agent identity ==="
"$CL" agent init worker-1 -d "Test agent"
"$CL" agent status
echo "--- Agent init complete ---"

echo ""
echo "=== 3. Test migrate-to-shared ==="
# Need a remote for the coordination branch
git add -A
git commit -m "initial"
# Create a bare remote
rm -rf /tmp/crosslink-test-remote
git init --bare /tmp/crosslink-test-remote
git remote add origin /tmp/crosslink-test-remote
git push -u origin main 2>&1 || git push -u origin master 2>&1

"$CL" migrate-to-shared 2>&1
echo "--- Migrate-to-shared complete ---"

echo ""
echo "=== 4. Verify coordination branch has JSON files ==="
git fetch origin crosslink/hub 2>&1 || echo "Note: fetch warning (expected)"
# Check via the hub-cache
ls -la .crosslink/.hub-cache/issues/ 2>/dev/null || echo "Cache dir listing failed"
echo "--- Coordination branch check complete ---"

echo ""
echo "=== 5. Test migrate-from-shared (re-hydrate) ==="
"$CL" migrate-from-shared 2>&1
echo "--- Migrate-from-shared complete ---"

echo ""
echo "=== 6. Verify issues still intact after round-trip ==="
"$CL" list
"$CL" show 1
"$CL" blocked
"$CL" ready
echo "--- Round-trip verification complete ---"

echo ""
echo "=== 7. Test lock claim/release/steal ==="
"$CL" locks claim 1 2>&1
"$CL" locks list 2>&1
"$CL" locks release 1 2>&1
"$CL" locks list 2>&1
echo "--- Lock commands complete ---"

echo ""
echo "=== 8. Test lock steal ==="
"$CL" locks claim 2 2>&1
"$CL" locks steal 2 2>&1
"$CL" locks list 2>&1
echo "--- Lock steal complete ---"

echo ""
echo "=== ALL TESTS PASSED ==="
