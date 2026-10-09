#!/usr/bin/python3
"""Stand-in for polkit-agent-helper-1 in proof harnesses.

Usage: tuna-polkit-helper-stub.py USER

Speaks the helper's line protocol: reads the cookie from stdin, asks for
the password with a PAM prompt, and answers SUCCESS for
$TUNA_POLKIT_TEST_PASSWORD (default "tuna-proof"), else FAILURE.
"""
import os
import sys

if len(sys.argv) != 2:
    sys.exit(2)
cookie = sys.stdin.readline()
print("PAM_PROMPT_ECHO_OFF Password: ", flush=True)
password = sys.stdin.readline().rstrip("\n")
want = os.environ.get("TUNA_POLKIT_TEST_PASSWORD", "tuna-proof")
print("SUCCESS" if cookie.strip() and password == want else "FAILURE", flush=True)
