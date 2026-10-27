# staged-runner

Self-hosted GitHub Actions runners on several hosts that take jobs in a fixed
order. Your fastest runner gets every job it can; the next runner starts only
when every runner before it is busy.

GitHub has no runner priority. A job goes to whichever matching runner asks
first, so a slow host takes jobs while a fast one sits idle. staged-runner
fixes that one thing by keeping each runner stopped until it is needed.

## What it does and does not do

This is a minimal tool. It does one thing: **it starts and stops runners you
already have, so that jobs spill over from one runner to the next in the
order you choose.**

It does not:

- register runners, hold GitHub tokens or call the GitHub API;
- create hosts, containers or runner images, or install the actions runner;
- look at the job queue, labels or repositories;
- scale to zero, make ephemeral runners or replace
  [actions-runner-controller](https://github.com/actions/actions-runner-controller)
  or similar systems.

It assumes **every runner in the fleet can take every job**: same labels, same
tools. If some jobs need a particular runner, give those their own label and
leave that runner out of the fleet.

## How it works

The fleet file lists your hosts, fastest first. Runners are ordered by number,
then by host:

    runner-A-1, runner-B-1, runner-C-1, runner-A-2, runner-B-2, ...

so each host's first runner comes before any host's second runner.

`staged-runner run <host>` runs on every host. Every 10 seconds it asks the
hosts before it for their runners' states, then for each of its own runners:

- **starts** it when every runner before it is busy or down;
- **stops** it when it is idle and some runner before it is idle or stopped;
- leaves a busy runner alone until its job is done.

The first runner always runs. A host that does not answer counts as busy, so
if your fast host goes down the next runners take over.

Some safeguards keep one broken runner from holding up the rest:

- A runner that stops within a minute of being started (a broken image, a
  registration GitHub has removed) is held as **down** for five minutes, so
  the runners after it take its jobs, and is then tried again.
- Every `docker` or `kill` command has a time limit. If the loop still stops
  making progress, the endpoint answers 503 and the other hosts count this
  host as down instead of trusting old states.
- If the endpoint stops accepting connections, staged-runner exits and the
  service manager restarts it.
- A host's answer counts only for that host's own runners.

Hosts tell each other one thing: their runners' states, as JSON at
`GET /status` on port 7391, for example
`{"host":"A","runners":{"runner-A-1":"busy"}}`.

A runner is stopped with SIGINT to its `Runner.Listener`. The runner then ends
its session with GitHub, so it gets no jobs while it is stopped. (A plain kill
leaves the session open, and GitHub keeps assigning it jobs for minutes.)

When work spills over, a queued job waits for the next check plus the runner's
start, usually 10 to 25 seconds.

"Idle" means the runner's process is running. staged-runner cannot see
whether GitHub has accepted its connection. A runner that runs but cannot
connect (see Troubleshooting) holds back the runners after it until it does.

## Setup

Each host needs the binary, its runners and a service. The steps below use
three hosts named `fast`, `slow` and `mac`; see
[`fleet.example.toml`](fleet.example.toml).

### 1. Install the binary

Download it from [Releases](../../releases) (Linux x86_64 and arm64, macOS
arm64 and x86_64; the Linux builds are static), or build it with Rust:

    cargo build --release --locked
    sudo install -m 755 target/release/staged-runner /usr/local/bin/

### 2. Write the fleet file

One file, the same on every host, at `/etc/staged-runner.toml` (or pass
`--config`).

| Key | Where | Meaning |
| --- | --- | --- |
| `listen` | top level | Address the status endpoint binds to. Default `0.0.0.0:7391`. |
| `poll` | top level | Seconds between checks. Default `10`. |
| `name` | `[[host]]` | The host's name; its runners are `runner-<name>-<n>`. Letters, digits, `-`, `_`, `.`; unique ignoring case. |
| `endpoint` | `[[host]]` | URL the other hosts reach it at, `http://address:port`. |
| `listen` | `[[host]]` | Optional: this host's bind address, overriding the top level. |
| `runners` | `[[host]]` | How many runners the host has, 1 to 64. |
| `kind` | `[[host]]` | `docker` or `native`, see below. |
| `root` | `[[host]]` | For `native` only: the absolute path of the folder that holds the runner folders. |

The order of the `[[host]]` entries is the spill-over order. Check it with:

    staged-runner plan

### 3. Set up the runners

Register each runner once, by hand, with the name staged-runner expects:
`runner-<host>-1`, `runner-<host>-2`, and so on. Give all of them the same
labels. Get a registration token from the repository or organisation settings
(Actions → Runners → New runner) or with
`gh api -X POST orgs/<org>/actions/runners/registration-token --jq .token`.

**`kind = "docker"`**: one container per runner, named after it. staged-runner
starts it with `docker start` and stops it with `docker stop`, so:

- create it with `--restart no`: staged-runner decides when it runs;
- its entrypoint must pass `docker stop`'s TERM on to the runner as INT, or
  the runner never ends its GitHub session. The image in
  [`examples/docker`](examples/docker) does this; copy its `entrypoint.sh`
  into your own image if you have one.

With the example image:

    docker build -t my-runner examples/docker
    docker create --name runner-slow-1 --restart no \
      -v runner-slow-1:/opt/runner \
      -e RUNNER_URL=https://github.com/<org> \
      -e RUNNER_NAME=runner-slow-1 \
      -e RUNNER_LABELS=my-label \
      -e RUNNER_TOKEN=<registration token> \
      my-runner

The runner registers on its first start and keeps its registration in the
named volume. The token expires after an hour and is not used again.

**`kind = "native"`**: an [actions runner](https://github.com/actions/runner/releases)
unpacked in `<root>/runner-<host>-<n>`, started with its `run.sh`. Output goes
to `staged-runner.log` in that folder. Use this on macOS, or on Linux without
Docker:

    mkdir -p /Users/ci/runners/runner-mac-1 && cd /Users/ci/runners/runner-mac-1
    curl -fsSL <runner tarball URL from the releases page> | tar -xz
    ./config.sh --url https://github.com/<org> --token <registration token> \
      --name runner-mac-1 --labels my-label --unattended

Do not install the runner's own service (`svc.sh`): staged-runner starts it.

### 4. Run the service

`staged-runner service <host>` prints a service definition for this system.
It runs as root for Docker runners on Linux, and otherwise as the account that
ran the command (with `sudo`, the account that ran `sudo`); change it with
`--user`. Native runners cannot run as root, and the command refuses to make
such a service. The account needs to be able to run `docker` for Docker
runners. Paths with spaces or special characters are refused; install the
binary and the fleet file somewhere plain.

Linux (systemd):

    staged-runner service slow | sudo tee /etc/systemd/system/staged-runner.service
    sudo systemctl daemon-reload
    sudo systemctl enable --now staged-runner
    journalctl -u staged-runner -f

macOS (launchd, starts at boot):

    staged-runner service mac | sudo tee /Library/LaunchDaemons/io.github.staged-runner.plist
    sudo launchctl bootstrap system /Library/LaunchDaemons/io.github.staged-runner.plist
    tail -f ~/Library/Logs/staged-runner.log

(as root, the log is `/var/log/staged-runner.log`).

### 5. Check it

    staged-runner status

asks every host and prints each runner's state in spill-over order. With
nothing running, the first runner is `idle` and the rest are `stopped`; on
GitHub's runners page only the first shows online. A stopped runner can take
up to a minute to show offline there.

## Adding or removing a host

To add one: set up its runners and the service (steps 1, 3 and 4), add its
`[[host]]` entry in its place in the order, copy the fleet file to every host
and restart staged-runner on each. Hosts read the file only at start.

To remove one: stop its service and its runners, take its entry out of the
fleet file, copy the file to the other hosts and restart them. Remove the
runners on GitHub if you will not use them again.

Stopping the service leaves the runners as they are: running ones keep
running.

## Troubleshooting

- **`no answer` in `staged-runner status`**: that host's service is not
  running, or its `endpoint` is not reachable from where you run the command.
  The other hosts treat it as busy.
- **A runner shows online on GitHub long after it stopped**: it was killed
  instead of stopped with INT, so its old session lives on. GitHub drops it
  after a few minutes. For Docker runners, check the entrypoint (step 3).
- **"A session for this runner already exists"** in a runner's log: the same
  stale session; the runner reconnects by itself once GitHub drops it. Until
  then it counts as idle and the runners after it stay stopped.
- **"stopped soon after it was started; holding it as down"**: the runner
  exits right after it starts. For a native runner, read `staged-runner.log`
  in its folder; for Docker, `docker logs <runner>`. A runner that stays
  offline on GitHub for 14 days is removed there and must be registered again.

## Limits

- A job can reach a runner in the moment before it is stopped. The runner
  then ends its session and the job fails or waits for another runner; re-run
  it. This is rare: a runner is checked for a job once more just before it
  is stopped.
- A tail runner that is rarely needed can sit stopped for weeks. GitHub
  removes runners that stay offline for 14 days; register it again if that
  happens.
- Each runner has exactly one place in the order. Weights, schedules and
  per-repository rules are out of scope.

## Security

The status endpoint is read-only, has no authentication and says only which
runners are busy. Bind it to a private network such as a VPN or tailnet, with
`listen` at the top of the fleet file or per host. Another host's answer is
trusted only for that host's own runners, but anyone who can reach the port
can see which runners are busy, and anyone who can pose as a host can make
the hosts after it start or keep runners stopped.

staged-runner holds no secrets. The runners' registrations stay where the
actions runner keeps them.

## License

MIT, see [LICENSE](LICENSE).
