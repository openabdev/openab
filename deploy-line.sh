#!/usr/bin/env bash
# deploy-line.sh — Deploy OpenAB with LINE adapter to openab-line-cluster EKS
# Usage: LINE_CHANNEL_ACCESS_TOKEN=xxx LINE_CHANNEL_SECRET=yyy bash deploy-line.sh
set -euo pipefail

REGION="${AWS_REGION:-us-east-1}"
CLUSTER_NAME="openab-line-cluster"
NAMESPACE="${NAMESPACE:-default}"
ACCOUNT_ID="256358067059"
ECR_URI="${ECR_URI:-${ACCOUNT_ID}.dkr.ecr.${REGION}.amazonaws.com/openab}"
IMAGE_TAG="${IMAGE_TAG:-line-latest}"

###############################################################################
# Required secrets
###############################################################################
GH_PAT_FILE="$HOME/.kiro/github-pat-linebot.key"
KIRO_KEY_FILE="$HOME/.kiro/my-kiro-worker1.key"
LINE_TOKEN_FILE="$HOME/.kiro/line-channel-access-token.key"
LINE_SECRET_FILE="$HOME/.kiro/line-channel-secret.key"

for f in "$GH_PAT_FILE" "$KIRO_KEY_FILE" "$LINE_TOKEN_FILE" "$LINE_SECRET_FILE"; do
  if [[ ! -f "$f" ]]; then
    echo "ERROR: $f not found." >&2
    exit 1
  fi
done

GH_PAT=$(cat "$GH_PAT_FILE")
KIRO_KEY=$(cat "$KIRO_KEY_FILE")
LINE_CHANNEL_ACCESS_TOKEN="${LINE_CHANNEL_ACCESS_TOKEN:-$(cat "$LINE_TOKEN_FILE")}"
LINE_CHANNEL_SECRET="${LINE_CHANNEL_SECRET:-$(cat "$LINE_SECRET_FILE")}"

###############################################################################
# Step 1: Create EKS cluster (idempotent — skips if exists)
###############################################################################
echo "==> Checking if cluster $CLUSTER_NAME exists..."
if ! aws eks describe-cluster --name "$CLUSTER_NAME" --region "$REGION" >/dev/null 2>&1; then
  echo "==> Creating EKS cluster $CLUSTER_NAME (this takes ~15 min)..."
  eksctl create cluster -f eksctl-line-cluster.yaml
else
  echo "==> Cluster $CLUSTER_NAME already exists, skipping creation."
fi

###############################################################################
# Step 2: Update kubeconfig
###############################################################################
echo "==> Updating kubeconfig for $CLUSTER_NAME..."
aws eks update-kubeconfig --name "$CLUSTER_NAME" --region "$REGION"

###############################################################################
# Step 3: Build and push Docker image
###############################################################################
echo "==> Logging into ECR..."
aws ecr get-login-password --region "$REGION" | \
  docker login --username AWS --password-stdin "${ACCOUNT_ID}.dkr.ecr.${REGION}.amazonaws.com"

echo "==> Building Docker image..."
docker build -t "openab:${IMAGE_TAG}" -f Dockerfile .

echo "==> Tagging and pushing to ECR..."
docker tag "openab:${IMAGE_TAG}" "${ECR_URI}:${IMAGE_TAG}"
docker push "${ECR_URI}:${IMAGE_TAG}"

###############################################################################
# Step 4: Create K8s secret for Kiro + GitHub credentials
###############################################################################
echo "==> Applying openab-line-kiro-keys secret..."
kubectl create secret generic openab-line-kiro-keys \
  --from-literal="GH_TOKEN=$GH_PAT" \
  --from-literal="KIRO_API_KEY=$KIRO_KEY" \
  --namespace "$NAMESPACE" \
  --dry-run=client -o yaml | kubectl apply -f -

###############################################################################
# Step 5: Helm install with LINE adapter
###############################################################################
echo "==> Helm upgrade/install..."
helm upgrade --install openab-line ./charts/openab \
  --namespace "$NAMESPACE" \
  --set image.repository="$ECR_URI" \
  --set image.tag="${IMAGE_TAG}" \
  --set image.pullPolicy="Always" \
  --set agents.kiro.discord.enabled=false \
  --set agents.kiro.line.enabled=true \
  --set agents.kiro.line.channelAccessToken="$LINE_CHANNEL_ACCESS_TOKEN" \
  --set agents.kiro.line.channelSecret="$LINE_CHANNEL_SECRET" \
  --set agents.kiro.line.webhookPort=8080 \
  --set 'agents.kiro.env.KIRO_API_KEY_OVERRIDE=${KIRO_API_KEY}' \
  --set agents.kiro.persistence.enabled=false \
  --set agents.kiro.resources.requests.cpu=200m \
  --set agents.kiro.resources.requests.memory=256Mi \
  --set agents.kiro.resources.limits.cpu=1 \
  --set agents.kiro.resources.limits.memory=512Mi \
  --set 'agents.kiro.envFrom[0].secretRef.name=openab-line-kiro-keys' \
  --set-file agents.kiro.agentsMd=agents-linebot.md

###############################################################################
# Step 6: Wait for rollout
###############################################################################
echo "==> Waiting for rollout..."
kubectl rollout status deployment/openab-line-kiro \
  --namespace "$NAMESPACE" --timeout=180s

echo "==> Done. Pod status:"
kubectl get pods -l app.kubernetes.io/name=openab -n "$NAMESPACE" -o wide

echo ""
echo "==> Next steps:"
echo "    1. Create a K8s Service (NodePort or LoadBalancer) to expose port 8080"
echo "    2. Update CloudFront origin (d1h5oqljvo7vv7.cloudfront.net) to point to the service"
echo "    3. Remove the CloudFront Function that returns 200 for POST"
echo "    4. LINE webhook URL stays: https://d1h5oqljvo7vv7.cloudfront.net"
