# Greeter VM acceptance (manual)

Scripted CI covers the conversation, picker, and failure paths
against the fake daemon. These runs need a real VM with real
credentials and stay manual.

## Cold boot to login screen

1. Boot the VM with greetd configured to launch `roost-greeter`.
2. Expect the login window with user list, session picker (Roost
   default), and no console visible.
3. Sign in with valid credentials; expect the selected session's
   first window to take input.

## Bad credentials

1. Enter a wrong password; expect a single generic failure notice.
2. Confirm no shell, console, or distinguishing error appears, and
   a second attempt works.

## Dead session

1. Point the Roost session entry at `false(1)`.
2. Sign in; expect return to the login screen with a plain-language
   notice, greeter process still alive.

## Real PAM factors

1. If the VM's PAM stack adds a second factor, confirm each prompt
   renders in turn and all are answerable from the greeter.
