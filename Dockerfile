# A container image for `msm`.
#
# Two stages: the first compiles, the second is what you actually run. The
# compiler, the source and the build cache together come to well over a
# gigabyte, and none of it is needed to run the finished program — so the
# second stage starts from a bare base image and copies in one binary.
#
# What this is for: running `msm` somewhere that is not your own machine — a
# streaming box you reach over ssh, or a server that keeps the chat log. `msm`
# is a terminal interface and nothing else: it takes no arguments, so the
# container always needs a terminal attached. See the notes at the bottom about
# logging in, which is the one part that does not work the same way in a
# container.

# ---------------------------------------------------------------------------
# Stage 1: build
# ---------------------------------------------------------------------------
# Pinned to the project's minimum supported Rust version rather than `latest`,
# so the image proves the MSRV is real. A newer compiler would happily accept
# code that does not build for someone on the version the project claims to
# support.
FROM rust:1.88-slim-bookworm AS build

WORKDIR /src

# Dependencies first, on their own layer. Copying the manifests and building a
# stub means this layer is only rebuilt when the dependencies change — editing
# a source file then recompiles this project alone rather than its entire
# dependency tree.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -rf src

COPY src ./src
# Cargo decides what to rebuild from file modification times, and the stub
# `main.rs` above has the same timestamp as the real one that just replaced
# it. Without this touch, cargo can conclude the binary is already up to date
# and ship the stub — an image that builds cleanly and does nothing.
RUN touch src/main.rs && cargo build --release

# ---------------------------------------------------------------------------
# Stage 2: run
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim

# `ca-certificates` is not optional: without it every HTTPS request to Twitch
# and Google fails certificate verification, which looks like a network fault
# and is not one.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# A normal user rather than root. Nothing here needs privilege, and a token
# file owned by root inside a container is awkward to read back out on the
# host.
RUN useradd --create-home --uid 1000 msm
USER msm
WORKDIR /home/msm

# Where the config, the saved logins and the log live. Mount a volume here to
# keep them between runs — without one, every `docker run` starts from nothing
# and asks you to log in again.
ENV MSM_CONFIG_DIR=/home/msm/.config/msm
VOLUME ["/home/msm/.config/msm"]

COPY --from=build /src/target/release/msm /usr/local/bin/msm

ENTRYPOINT ["msm"]

# ---------------------------------------------------------------------------
# Using it
# ---------------------------------------------------------------------------
#
#   docker build -t msm .
#
# The interface needs a terminal, so it needs `-it`, and it needs somewhere to
# keep its config:
#
#   docker run -it -v msm-config:/home/msm/.config/msm msm
#
# **Logging in is the awkward part.** Authorising an account opens a browser
# and waits for the platform to redirect back to a port on localhost — and
# "localhost" inside a container is the container, not your desktop. Two ways
# round it:
#
# 1. Log in on your own machine first — run `msm` there and authorise both
#    platforms on its "Authorise your accounts" screen — then mount the config
#    directory that produced, which already holds the tokens. This is the
#    simpler option and the one to reach for.
#
# 2. Publish the callback port so the redirect can reach the container, then
#    open the printed URL in your own browser and log in from the Accounts
#    section of the container's Config tab:
#
#      docker run -it -p 8017:8017 -v msm-config:/home/msm/.config/msm msm
#
#    This only works if the redirect URL registered in the developer console
#    matches the port, and it puts an OAuth callback listener on your network
#    for the duration — so prefer option 1 unless you have a reason not to.
#
# With a config that already holds tokens, everything else happens inside the
# interface: alt+1 to set the title and go live, alt+5 for configuration,
# accounts and diagnostics.
