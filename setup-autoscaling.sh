#!/usr/bin/env bash
# setup-autoscaling.sh — Enable Pod (HPA) + Node (Cluster Autoscaler) autoscaling
# Run AFTER deploy-line.sh has completed successfully.
# Usage: bash setup-autoscaling.sh
set -euo pipefail

REGION="${AWS_REGION:-us-east-1}"
CLUSTER_NAME="openab-line-cluster"
NAMESPACE="${NAMESPACE:-default}"
ACCOUNT_ID="256358067059"
ECR_URI="${ECR_URI:-${ACCOUNT_ID}.dkr.ecr.${REGION}.amazonaws.com/openab}"
IMAGE_TAG="${IMAGE_TAG:-line-latest}"

###############################################################################
# Step 1: Install metrics-server (required for HPA)
###############################################################################
echo "==> Installing metrics-server..."
if kubectl get deployment metrics-server -n kube-system >/dev/null 2>&1; then
  echo "    metrics-server already installed, skipping."
else
  kubectl apply -f https://github.com/kubernetes-sigs/metrics-server/releases/latest/download/components.yaml
  echo "    Waiting for metrics-server to be ready..."
  kubectl rollout status deployment/metrics-server -n kube-system --timeout=120s
fi

###############################################################################
# Step 2: Install Cluster Autoscaler (node-level scaling)
###############################################################################
echo "==> Setting up Cluster Autoscaler..."

# Create IAM OIDC provider
echo "    Associating IAM OIDC provider..."
eksctl utils associate-iam-oidc-provider \
  --cluster "$CLUSTER_NAME" --region "$REGION" --approve

# Create IAM service account
echo "    Creating IAM service account for cluster-autoscaler..."
eksctl create iamserviceaccount \
  --cluster "$CLUSTER_NAME" \
  --namespace kube-system \
  --name cluster-autoscaler \
  --attach-policy-arn arn:aws:iam::aws:policy/AutoScalingFullAccess \
  --approve \
  --region "$REGION" \
  --override-existing-serviceaccounts

# Install via Helm
echo "    Installing cluster-autoscaler Helm chart..."
helm repo add autoscaler https://kubernetes.github.io/autoscaler 2>/dev/null || true
helm repo update
helm upgrade --install cluster-autoscaler autoscaler/cluster-autoscaler \
  --namespace kube-system \
  --set autoDiscovery.clusterName="$CLUSTER_NAME" \
  --set awsRegion="$REGION" \
  --set rbac.serviceAccount.create=false \
  --set rbac.serviceAccount.name=cluster-autoscaler

###############################################################################
# Step 3: Helm upgrade — disable PVC + add resource requests for HPA
###############################################################################
echo "==> Upgrading Helm release with autoscaling-friendly settings..."
helm upgrade openab-line ./charts/openab \
  --namespace "$NAMESPACE" \
  --reuse-values \
  --set agents.kiro.persistence.enabled=false \
  --set agents.kiro.resources.requests.cpu=200m \
  --set agents.kiro.resources.requests.memory=256Mi \
  --set agents.kiro.resources.limits.cpu=1 \
  --set agents.kiro.resources.limits.memory=512Mi

echo "==> Waiting for rollout..."
kubectl rollout status deployment/openab-line-kiro \
  --namespace "$NAMESPACE" --timeout=180s

###############################################################################
# Step 4: Apply HPA
###############################################################################
echo "==> Applying HPA..."
kubectl apply -f k8s/hpa.yaml -n "$NAMESPACE"

###############################################################################
# Done
###############################################################################
echo ""
echo "==> Autoscaling is ready!"
echo "    Pod-level:  HPA (1-5 replicas, target 60% CPU)"
echo "    Node-level: Cluster Autoscaler (1-4 nodes, t3.medium)"
echo ""
echo "    Verify with:"
echo "      kubectl get hpa"
echo "      kubectl top pods"
echo "      kubectl get nodes"
