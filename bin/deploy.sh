#!/usr/bin/env bash
# Ship HEAD to the cluster: wait for its image, pin it, reconcile, verify.
#
#   bin/deploy.sh
#
# Exists because doing this by hand went wrong in every way it could: waiting
# on the newest CI run instead of THIS commit's (which right after a push is
# still the previous build), pinning :latest that spegel then served from a
# stale node cache, and calling it deployed after `git push` when the image had
# not been built at all. Each of those reported success while shipping nothing.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SHA=$(git rev-parse HEAD)
LAB="${TRIGGERLAB:-$HOME/dev/magnus/triggerlab}"
export KUBECONFIG="${KUBECONFIG:-$LAB/kubeconfig}"
say() { echo "[$(date -u +%H:%M:%SZ)] $*"; }

# The image is built by CI from the pushed SHA, so local edits never reach the
# cluster - but a dirty tree usually means the change you think you are
# shipping is still sitting here. Say which files, and let an explicit flag
# through: this repo sometimes has another agent's work in flight beside yours,
# and their half-finished file is not a reason to block your deploy.
if [ -n "$(git status --porcelain)" ]; then
  echo "deploy: uncommitted changes (the image is built from ${SHA:0:7}, so these will NOT ship):" >&2
  git status --porcelain | sed 's/^/  /' >&2
  if [ "${1:-}" != "--allow-dirty" ]; then
    echo "deploy: commit them, or re-run with --allow-dirty if they are not yours" >&2
    exit 1
  fi
  echo "deploy: --allow-dirty given, continuing" >&2
fi

# A committed-but-unpushed HEAD is the same failure wearing a clean shirt: the
# tree looks fine, CI never sees the commit, and the wait below times out
# blaming a path filter. Push it rather than explaining it.
if ! git ls-remote --exit-code origin "$SHA" >/dev/null 2>&1 &&
   [ -n "$(git log --oneline @{u}..HEAD 2>/dev/null)" ]; then
  say "pushing ${SHA:0:7}"
  git push -q
fi

say "waiting for the image for ${SHA:0:7}"
for _ in $(seq 1 80); do
  read -r status conclusion <<<"$(gh run list -R MrTrigger/ai-trader --workflow paper-image \
      --limit 15 --json headSha,status,conclusion \
      -q ".[] | select(.headSha==\"$SHA\") | .status + \" \" + (.conclusion // \"-\")" | head -1)"
  [ "${status:-}" = "completed" ] && break
  sleep 20
done
if [ "${status:-}" != "completed" ]; then
  echo "deploy: no completed build for ${SHA:0:7} - did a path filter skip it? see .github/workflows/image.yaml" >&2
  exit 1
fi
[ "$conclusion" = "success" ] || { echo "deploy: build for ${SHA:0:7} concluded $conclusion" >&2; exit 1; }

say "pinning the cluster to ${SHA:0:7}"
cd "$LAB"
sed -i "s|aitrader-paper:[0-9a-f]\{40\}|aitrader-paper:$SHA|g" \
  kubernetes/apps/trader/aitrader-paper/app/deployment.yaml \
  kubernetes/apps/trader/aitrader-paper/app/cronjob.yaml
kubectl kustomize kubernetes/apps/trader/aitrader-paper/app >/dev/null
if [ -n "$(git status --porcelain kubernetes/apps/trader/aitrader-paper)" ]; then
  git add kubernetes/apps/trader/aitrader-paper
  git commit -q -m "aitrader-paper: $SHA"
  git push -q
else
  say "already pinned"
fi

say "reconciling"
kubectl annotate gitrepository flux-system -n flux-system \
  reconcile.fluxcd.io/requestedAt="$(date +%s)" --overwrite >/dev/null
sleep 20
kubectl annotate kustomization aitrader-paper -n trader \
  reconcile.fluxcd.io/requestedAt="$(date +%s)" --overwrite >/dev/null
sleep 10
kubectl rollout status deployment/aitrader-paper -n trader --timeout=600s

# Verify what is RUNNING, not what was asked for. The whole point.
#
# Selected by NAME, not by index: containers[1] was the api until the pod grew
# an IB Gateway sidecar, and an index that silently points at a different
# container verifies the wrong thing while reporting success.
running=$(kubectl get pod -n trader -l app.kubernetes.io/name=aitrader-paper \
  --field-selector=status.phase=Running \
  -o jsonpath='{.items[0].spec.containers[?(@.name=="api")].image}')
if [ "$running" != "ghcr.io/mrtrigger/aitrader-paper:$SHA" ]; then
  echo "deploy: pod is running $running, expected :$SHA" >&2
  exit 1
fi
say "running ${SHA:0:7} - https://trader.wallintech.eu"
