# Data Structures & Interfaces (body)

- `Prompt`: one auth question from the daemon — id, echo/non-echo
  style, message text. The unit the state machine renders and answers.
- `AuthExchange`: ordered prompts plus terminal outcome
  (success with session command, or generic failure). Never carries
  distinguishing failure detail.
- `SessionEntry`: name, exec command, desktop-file source, and
  is-default flag. Produced by the enumerator from session files.
- `GreeterModel`: selected user, selected session, prompt queue,
  notice text, screen state (user-pick / prompting / failed /
  launching). The single state the UI renders from.
- Greeter↔daemon boundary: the adopted daemon's JSON-IPC conversation
  (create-session → post-auth-message* → start; cancel on failure).
  Exact wire shapes verified against greetd 0.10.3 at implementation.
- Greeter→session boundary: spawned session command observed by the
  supervisor; early exit maps to the dead-session notice, never to
  daemon internals leaking into the UI.
