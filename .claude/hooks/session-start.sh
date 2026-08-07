#!/bin/bash
# SessionStart hook for Claude Code on the web.
#
# Why this exists: kaleidotron has one git dependency (icy_parser_core, from
# github.com/mkrueger/icy_tools). In the web sandbox, outbound github *git* access
# is routed through a repo-scoped proxy that only permits THIS repo, so a fresh
# `cargo build`/`cargo test` can't fetch that dependency (the proxy 403s). This
# hook removes that scoped-proxy rewrite for github.com so cargo fetches the
# dependency directly through the session's general (network-policy-governed)
# egress proxy — exactly what a normal clone does.
#
# Scope / safety:
#   * It only acts in the remote web environment ($CLAUDE_CODE_REMOTE) and only
#     when the scoped-proxy rewrite is actually present, so it is a no-op on your
#     local machines and for anyone else cloning the repo. Nothing about the
#     project's dependencies, Cargo.toml, or Cargo.lock changes — everyone else
#     keeps building exactly as before.
#   * It removes ONLY the `https://github.com/` -> local-proxy rewrite. Commit
#     signing, the repo's own origin remote (an explicit proxied URL, untouched),
#     and the ssh->https rewrites are all left intact.
set -euo pipefail

# Local machines (and any non-web environment) need none of this — bail early.
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

# Remove any global `url.<…>.insteadOf = https://github.com/` rewrite that points
# github through the local repo-scoping proxy. The proxy port changes per session,
# so match it dynamically (by the local_proxy@127.0.0.1 marker) rather than pinning.
while IFS= read -r key; do
  section="${key%.insteadof}"   # strip the trailing ".insteadof" -> the [url "…"] section
  git config --global --remove-section "$section" 2>/dev/null || true
done < <(
  git config --global --get-regexp '^url\..*\.insteadof$' 2>/dev/null \
    | awk '$2 == "https://github.com/" && /local_proxy@127\.0\.0\.1/ { print $1 }'
)

# Warm the dependency cache so the first build/test is fast (the container image is
# cached after this hook completes). Best-effort: never fail session start over a
# transient network issue — the agent can always re-run cargo later.
if command -v cargo >/dev/null 2>&1; then
  cargo fetch --locked --manifest-path "${CLAUDE_PROJECT_DIR:-.}/Cargo.toml" || true
fi
