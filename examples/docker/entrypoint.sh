#!/usr/bin/env bash
# Registers the runner on its first start, then runs it. Keep /opt/runner on a
# volume so the registration survives the container being recreated.
#
# docker stop sends TERM. The runner ends its session with GitHub only on INT,
# so TERM is passed on as INT; otherwise GitHub would keep handing the stopped
# runner jobs for several minutes.
set -euo pipefail
trap 'exit 143' TERM  # until the runner is up
if [ ! -f .runner ]; then
  ./config.sh --unattended --url "${RUNNER_URL:?}" --token "${RUNNER_TOKEN:?}" \
    --name "${RUNNER_NAME:?}" --labels "${RUNNER_LABELS:-self-hosted}" --work _work
fi
unset RUNNER_TOKEN  # jobs do not need it
set -m  # else a background job starts with INT ignored
./run.sh &
trap 'pkill -INT -f "[b]in/Runner.Listener"' TERM
wait $! || wait $!
