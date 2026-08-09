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

if [ -n "$(git status --porcelain)" ]; then
  echo "deploy: working tree is dirty - commit first, or the cluster runs code that is not in git" >&2
  exit 1
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
running=$(kubectl get pod -n trader -l app.kubernetes.io/name=aitrader-paper \
  --field-selector=status.phase=Running \
  -o jsonpath='{.items[0].spec.containers[1].image}')
if [ "$running" != "ghcr.io/mrtrigger/aitrader-paper:$SHA" ]; then
  echo "deploy: pod is running $running, expected :$SHA" >&2
  exit 1
fi
say "running ${SHA:0:7} - https://trader.wallintech.eu"
