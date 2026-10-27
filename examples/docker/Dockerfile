# A minimal runner image for kind = "docker". Add the tools your jobs need.
FROM ubuntu:24.04
ARG RUNNER_VERSION=2.338.2
ARG TARGETARCH
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git procps \
 && userdel -r ubuntu \
 && useradd -m -u 1000 runner \
 && arch=$([ "$TARGETARCH" = arm64 ] && echo arm64 || echo x64) \
 && mkdir /opt/runner \
 && curl -fsSL "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-${arch}-${RUNNER_VERSION}.tar.gz" \
    | tar -xz -C /opt/runner \
 && /opt/runner/bin/installdependencies.sh \
 && chown -R runner:runner /opt/runner \
 && rm -rf /var/lib/apt/lists/*
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
USER runner
WORKDIR /opt/runner
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
