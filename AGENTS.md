# AGENTS.md

Rules for agents working in this repository.

## What this is

staged-runner starts and stops self-hosted GitHub Actions runners on several
hosts so that jobs spill over from one runner to the next in a fixed order.
It is one Rust binary (edition 2024, rust-version 1.85).

- `src/main.rs`: the command line (`run`, `plan`, `status`, `service`).
- `src/config.rs`: the fleet file (`fleet.example.toml`) and its checks.
- `src/stages.rs`: the spill-over order and the start and stop decisions.
- `src/agent.rs`: the loop each host runs and the `GET /status` endpoint.
- `src/local.rs`: starting, stopping and reading the state of this host's
  runners (`docker` or `native`).
- `src/service.rs`: the systemd unit and launchd plist `service` prints.
- `examples/docker`: an example runner image and its entrypoint.
- `README.md`: the user guide. Keep it in step with behaviour.

## Rules

- Keep it minimal. The README's "What it does and does not do" is the scope:
  no GitHub API calls, no tokens, no runner registration, no job queue,
  labels or scaling. Do not add those.
- staged-runner holds no secrets. Never add any to the repository.
- Runners are stopped with SIGINT to `Runner.Listener` (TERM passed on as
  INT in Docker) so they end their GitHub session. Do not change that to a
  plain kill.
- Every `docker` or `kill` command keeps a time limit. A host that does not
  answer counts as busy; another host's answer counts only for its own
  runners. Keep those safeguards.
- The status endpoint is read-only and says only which runners are busy.
- Tests must not need Docker, a runner, the network or a second host.
- When behaviour or the fleet file changes, update `README.md` and
  `fleet.example.toml` in the same change.
- Dependencies stay few; build with `--locked`.
- Open a pull request and stop. Never push to the default branch.

## Verify

`.pi/verify-cmd` is the fast gate. Run it before you say a change is done.
It runs what CI runs: `cargo fmt --check`, `cargo clippy --locked
--all-targets -- -D warnings` and `cargo test --locked`.

## Commits

A short subject in the imperative, capitalised, no prefix and no trailing
period ("Order runners host by host"). A plain body says what changed and
why. A release bumps the version in `Cargo.toml` and says so in the body.
