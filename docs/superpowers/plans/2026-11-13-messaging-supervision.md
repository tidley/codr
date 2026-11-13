# Supervise embedded messaging

The combined process previously kept its bridge loop alive after its MDK worker
exited. Systemd therefore saw an active process and never used Restart=on-failure.

Use an in-process completion channel, held by the worker for its whole lifetime.
Channel closure wakes the main select on success, error or unwinding panic. On
unexpected completion, save bridge state, join the worker through normal shutdown
and return failure. Do not poll, start a second runtime, alter accounts, or add a
second retry policy. A run without embedded messaging has no completion watcher.
Normal SIGTERM remains successful. Log only the existing privacy-safe error code
and make the supervisor-recovery outcome explicit; do not log nested error text.

Tests reproduce the old process staying alive on invalid startup storage, then
check startup failure, socket loss after readiness, healthy account-free startup,
external socket refusal and graceful termination. Use a short synthetic socket
basename within the normal MDK dev directory to fit the fastbuild temporary root.
This changes test fixtures, not storage-adapter configuration.

Run the Rust and Python suites, strict Clippy and formatting. Get two independent
read-only reviews before deployment. Record the change under 0.12.0 in development.
After owner-authorized backup, deploy and restart Codr only; verify account/chat/
receipt/draft/session preservation. The original generic app_error is not yet
identified, and this fix does not claim to cure every startup or network failure.
