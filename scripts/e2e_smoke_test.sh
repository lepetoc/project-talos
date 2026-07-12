#!/usr/bin/env bash
#
# End-to-end smoke test for the `api` server. Drives the actual compiled
# binary over HTTP with curl (no test harness, no in-memory database) to
# exercise registration, auth, zones, and arm/disarm, and — the main point
# of this script — persistence and zone replay across a real process
# restart against the same on-disk SQLite file.
#
# Does not exercise zone-triggering, EntryDelay, or Triggered: no external
# interface can report a zone event yet, so those states aren't reachable
# from outside the running process. Does not check the WebSocket endpoint
# either; that already has client/server tests elsewhere.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

HOST="127.0.0.1"
PORT="3000"
BASE_URL="http://${HOST}:${PORT}"
DB_FILE="$SCRIPT_DIR/.e2e_smoke_test.db"
BINARY="$REPO_ROOT/target/debug/api"

export TALOS_JWT_SECRET="e2e-smoke-test-secret"
export TALOS_EXIT_DELAY_SECS=2
export TALOS_ENTRY_DELAY_SECS=2
export TALOS_DATABASE_URL="sqlite://${DB_FILE}"

SERVER_PID=""
STEPS_PASSED=()

step_pass() {
    STEPS_PASSED+=("$1")
    echo "[PASS] $1"
}

print_summary() {
    local result="$1"
    echo ""
    echo "=== Smoke test ${result} ==="
    local step
    for step in "${STEPS_PASSED[@]}"; do
        echo "  [ok] $step"
    done
}

die() {
    echo "[FAIL] $1" >&2
    print_summary "FAILED"
    echo "  [FAIL] $1"
    exit 1
}

stop_server() {
    if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null
        wait "$SERVER_PID" 2>/dev/null
    fi
    SERVER_PID=""
}

cleanup() {
    stop_server
    rm -f "$DB_FILE"
}
trap cleanup EXIT

start_server() {
    "$BINARY" &
    SERVER_PID=$!
}

wait_for_health() {
    local attempt
    for attempt in $(seq 1 40); do
        if [[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE_URL/health" 2>/dev/null)" == "200" ]]; then
            return 0
        fi
        if ! kill -0 "$SERVER_PID" 2>/dev/null; then
            die "server process exited before becoming healthy"
        fi
        sleep 0.5
    done
    die "server did not become healthy within timeout"
}

# Issues an HTTP request via curl. Prints the response body followed by a
# trailing line holding the HTTP status code; callers split on the last
# newline. $1=method $2=path $3=token (or "") $4=json body (or "")
http() {
    local method="$1" path="$2" token="$3" data="$4"
    local args=(-s -w $'\n%{http_code}' -X "$method" "$BASE_URL$path")
    if [[ -n "$token" ]]; then
        args+=(-H "Authorization: Bearer $token")
    fi
    if [[ -n "$data" ]]; then
        args+=(-H "Content-Type: application/json" -d "$data")
    fi
    curl "${args[@]}"
}

http_status() {
    local response="$1"
    echo "${response##*$'\n'}"
}

http_body() {
    local response="$1"
    echo "${response%$'\n'*}"
}

extract_token() {
    echo "$1" | grep -o '"token":"[^"]*"' | head -1 | sed -E 's/"token":"([^"]*)"/\1/'
}

assert_status() {
    local description="$1" expected="$2" response="$3"
    local actual
    actual="$(http_status "$response")"
    if [[ "$actual" != "$expected" ]]; then
        die "$description: expected HTTP $expected, got $actual (body: $(http_body "$response"))"
    fi
}

assert_contains() {
    local description="$1" haystack="$2" needle="$3"
    if [[ "$haystack" != *"$needle"* ]]; then
        die "$description: expected response to contain '$needle', got: $haystack"
    fi
}

assert_not_contains() {
    local description="$1" haystack="$2" needle="$3"
    if [[ "$haystack" == *"$needle"* ]]; then
        die "$description: expected response NOT to contain '$needle', got: $haystack"
    fi
}

if [[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE_URL/health" 2>/dev/null)" == "200" ]]; then
    die "something is already listening on $BASE_URL/health — refusing to run against it"
fi

echo "Building api binary..."
(cd "$REPO_ROOT" && cargo build --package api) || die "cargo build --package api failed"
step_pass "built api binary"

rm -f "$DB_FILE"

# --- 1. Start the server and wait for /health ---
start_server
wait_for_health
step_pass "server started and became healthy"

# --- 2. Register the first user (no token), then log in and capture the token ---
response="$(http POST /auth/register "" '{"username":"alice","password":"hunter2"}')"
assert_status "register first user (alice, no token)" 201 "$response"
step_pass "registered first user (alice) without a token"

response="$(http POST /auth/login "" '{"username":"alice","password":"hunter2"}')"
assert_status "login as alice" 200 "$response"
ALICE_TOKEN="$(extract_token "$(http_body "$response")")"
[[ -n "$ALICE_TOKEN" ]] || die "login as alice did not return a token"
step_pass "logged in as alice and captured token"

# --- 3. Second user rejected without a token, accepted with alice's token ---
response="$(http POST /auth/register "" '{"username":"bob","password":"hunter3"}')"
assert_status "register second user (bob, no token)" 401 "$response"
step_pass "registering second user without a token was rejected (401)"

response="$(http POST /auth/register "$ALICE_TOKEN" '{"username":"bob","password":"hunter3"}')"
assert_status "register second user (bob, with alice's token)" 201 "$response"
step_pass "registered second user with alice's token"

# --- 4. Create two zones and confirm both list as Clear ---
response="$(http POST /zones "$ALICE_TOKEN" '{"id":1,"kind":"Delay"}')"
assert_status "create zone 1 (Delay)" 201 "$response"
response="$(http POST /zones "$ALICE_TOKEN" '{"id":2,"kind":"Instant"}')"
assert_status "create zone 2 (Instant)" 201 "$response"
step_pass "created a Delay zone and an Instant zone"

response="$(http GET /zones "$ALICE_TOKEN" "")"
assert_status "list zones" 200 "$response"
body="$(http_body "$response")"
assert_contains "list zones" "$body" '{"id":1,"kind":"Delay","status":"Clear"}'
assert_contains "list zones" "$body" '{"id":2,"kind":"Instant","status":"Clear"}'
step_pass "GET /zones lists both zones as Clear"

# --- 5. Arm, then poll /state until Armed ---
response="$(http POST /arm "$ALICE_TOKEN" "")"
assert_status "arm" 200 "$response"
step_pass "POST /arm accepted"

armed=""
for attempt in $(seq 1 20); do
    response="$(http GET /state "$ALICE_TOKEN" "")"
    assert_status "get state while polling for Armed" 200 "$response"
    if [[ "$(http_body "$response")" == '{"state":"Armed"}' ]]; then
        armed="yes"
        break
    fi
    sleep 0.5
done
[[ -n "$armed" ]] || die "state did not reach Armed within timeout after exit delay"
step_pass "GET /state reported Armed after the exit delay"

# --- 6. Disarm and confirm state ---
response="$(http POST /disarm "$ALICE_TOKEN" "")"
assert_status "disarm" 200 "$response"
assert_contains "disarm response" "$(http_body "$response")" '"state":"Disarmed"'
step_pass "POST /disarm accepted"

response="$(http GET /state "$ALICE_TOKEN" "")"
assert_status "get state after disarm" 200 "$response"
assert_contains "state after disarm" "$(http_body "$response")" '"state":"Disarmed"'
step_pass "GET /state reported Disarmed"

# --- 7. Delete one zone and confirm it disappears ---
response="$(http DELETE /zones/2 "$ALICE_TOKEN" "")"
assert_status "delete zone 2" 204 "$response"
step_pass "deleted zone 2"

response="$(http GET /zones "$ALICE_TOKEN" "")"
assert_status "list zones after delete" 200 "$response"
body="$(http_body "$response")"
assert_contains "list zones after delete" "$body" '{"id":1,"kind":"Delay","status":"Clear"}'
assert_not_contains "list zones after delete" "$body" '"id":2'
step_pass "GET /zones no longer lists the deleted zone"

# --- 8. Restart the server against the same database and confirm persistence ---
stop_server
step_pass "stopped the server"

start_server
wait_for_health
step_pass "restarted the server against the same database file and it became healthy"

response="$(http POST /auth/login "" '{"username":"alice","password":"hunter2"}')"
assert_status "fresh login as alice after restart" 200 "$response"
FRESH_TOKEN="$(extract_token "$(http_body "$response")")"
[[ -n "$FRESH_TOKEN" ]] || die "fresh login as alice after restart did not return a token"
step_pass "logged in fresh as alice after restart"

response="$(http GET /zones "$FRESH_TOKEN" "")"
assert_status "list zones after restart" 200 "$response"
body="$(http_body "$response")"
if [[ "$body" != '[{"id":1,"kind":"Delay","status":"Clear"}]' ]]; then
    die "expected exactly the one remaining zone after restart, got: $body"
fi
step_pass "GET /zones after restart shows exactly the one remaining zone (persistence verified)"

# --- 9. Stop the server and remove the disposable database file ---
stop_server
rm -f "$DB_FILE"
step_pass "stopped the server and removed the disposable database file"

print_summary "PASSED"
exit 0
