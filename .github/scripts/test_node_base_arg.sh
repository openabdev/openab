#!/usr/bin/env bash
# Regression tests for node_base_arg.sh. Run from the repository root:
#   .github/scripts/test_node_base_arg.sh
set -euo pipefail

script=.github/scripts/node_base_arg.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failures=0

# expect <name> <want_rc> <want_stdout> <want_stderr_substring> <dockerfile_body> [script args...]
# The Dockerfile body is written to a fixture; "@" in args is replaced by its path.
expect() {
  local name=$1 want_rc=$2 want_out=$3 want_err=$4 body=$5
  shift 5
  local file="$tmp/$name.Dockerfile" args=() rc=0 out err
  printf '%b' "$body" > "$file"
  for a in "$@"; do args+=("${a//@/$file}"); done
  out=$("$script" "${args[@]}" 2> "$tmp/err") || rc=$?
  err=$(cat "$tmp/err")
  if [ "$rc" != "$want_rc" ] || [ "$out" != "$want_out" ] || [[ "$err" != *"$want_err"* ]]; then
    echo "FAIL $name: rc=$rc (want $want_rc) out=[$out] (want [$want_out]) err=[$err] (want *$want_err*)"
    failures=$((failures + 1))
  else
    echo "ok   $name"
  fi
}

# Resolution of pinned bases
expect stage-chain     0 NODE22_VERSION '' 'FROM node:${NODE22_VERSION}-trixie-slim AS base-node\nFROM base-node AS claude\n' @ claude
expect last-stage      0 NODE24_VERSION '' 'FROM rust AS builder\nFROM node:${NODE24_VERSION}-trixie-slim\n' @
expect flags-and-case  0 NODE22_VERSION '' 'from --platform=$BUILDPLATFORM node:${NODE22_VERSION}-trixie-slim as base\nFROM base As app\n' @ app
expect stage-name-case 0 NODE22_VERSION '' 'FROM node:${NODE22_VERSION}-slim AS Base\nFROM base AS app\n' @ app
expect target-case     0 NODE22_VERSION '' 'FROM node:${NODE22_VERSION}-slim AS App\n' @ app
expect registry-pinned 0 NODE22_VERSION '' 'FROM docker.io/library/node:${NODE22_VERSION}-slim AS app\n' @ app
expect digest-on-pin   0 NODE22_VERSION '' 'FROM node:${NODE22_VERSION}-slim@sha256:0000 AS app\n' @ app
expect non-node        0 ''             '' 'FROM debian:trixie-slim AS base\nFROM base AS app\n' @ app
expect allowed-var     0 ''             '' 'FROM ${BUILDER_IMAGE} AS builder\nFROM debian AS app\n' @ app
expect allowed-var-stage 0 ''           '' 'FROM rust AS local_builder\nARG BUILDER_IMAGE=local_builder\nFROM ${BUILDER_IMAGE} AS builder\n' @ builder
expect allowed-var-node  1 '' 'is not pinned' 'ARG BUILDER_IMAGE=node:22\nFROM ${BUILDER_IMAGE} AS app\n' @ app
expect allowed-var-loop  1 '' 'refers back'   'ARG BUILDER_IMAGE=${BUILDER_IMAGE}\nFROM ${BUILDER_IMAGE} AS app\n' @ app
expect param-expansion 1 '' 'cannot be resolved' 'FROM ${BASE:-node:22-trixie-slim} AS app\n' @ app
expect bare-composite  1 '' 'cannot be resolved' 'FROM $BASE-node AS app\n' @ app

# Fail closed on Node.js bases that are not pinned through the ARG
expect literal-tag     1 '' 'is not pinned'      'FROM node:22-trixie-slim AS app\n' @ app
expect digest-only     1 '' 'is not pinned'      'FROM node@sha256:0000 AS app\n' @ app
expect registry-tag    1 '' 'is not pinned'      'FROM docker.io/library/node:22-trixie-slim AS app\n' @ app
expect library-tag     1 '' 'is not pinned'      'FROM library/node:22-trixie-slim AS app\n' @ app
expect bare-node       1 '' 'is not pinned'      'FROM node AS app\n' @ app
expect missing-suffix  1 '' 'has no tag suffix'  'FROM node:${NODE22_VERSION} AS app\n' @ app
expect var-base        1 '' 'from a build arg'   'ARG BASE=node:22-slim\nFROM ${BASE} AS app\n' @ app
expect var-base-plain  1 '' 'from a build arg'   'ARG BASE=node:22-slim\nFROM $BASE AS app\n' @ app
expect var-in-tag      1 '' 'cannot be resolved' 'FROM node${TAG}-slim AS app\n' @ app
expect var-image-name  1 '' 'cannot be resolved' 'FROM ${IMG}:22 AS app\n' @ app
expect var-registry    1 '' 'cannot be resolved' 'FROM ${REG}/node:${NODE22_VERSION}-slim AS app\n' @ app
expect var-node-tag    1 '' 'cannot be resolved' 'FROM node:${TAG} AS app\n' @ app
expect hidden-stage    1 '' 'is not pinned'      'FROM node:22-slim AS Base\nFROM base AS mid\nFROM debian AS app\n' @ app
expect index-registry  1 '' 'is not pinned'      'FROM index.docker.io/library/node:22 AS app\n' @ app
expect mirror-registry 1 '' 'is not pinned'      'FROM mirror.gcr.io/library/node:22 AS app\n' @ app
expect ecr-mirror      1 '' 'is not pinned'      'FROM public.ecr.aws/docker/library/node:22 AS app\n' @ app
expect continued-bad   1 '' 'is not pinned'      'FROM \\\n  node:22-slim AS app\n' @ app
expect continued-cmt   1 '' 'is not pinned'      'FROM --platform=linux/amd64 \\\n# a comment\n  node:22-slim \\\n  AS app\n' @ app
expect empty-from      1 '' 'without an image'   'FROM\n' @
expect escape-directive 1 '' 'escape parser'     '# escape=`\nFROM debian AS app\n' @ app
expect unknown-target  1 '' 'not found'          'FROM debian AS app\n' @ nope
expect continued-ok    0 NODE24_VERSION ''       'FROM \\\n  node:${NODE24_VERSION}-trixie-slim \\\n  AS app\n' @ app
expect other-repo      0 ''             ''       'FROM ghcr.io/openabdev/openab-native:beta AS app\nFROM rust:1@sha256:00 AS b\n' @ app

# --check mode: every stage, one line per pin used
expect check-pins      0 "$tmp/check-pins.Dockerfile NODE22_VERSION" '' \
  'FROM node:${NODE22_VERSION}-slim AS a\nFROM a AS b\nFROM debian AS c\n' --check @
expect check-bad       1 '' 'is not pinned' 'FROM debian AS a\nFROM --platform=linux/amd64 node:22 AS b\n' --check @

# Current repository Dockerfiles resolve exactly as the old hand-written allowlist did.
# repo_expect checks the exit status too, so a crash cannot pass as "not Node-based".
repo_expect() { # repo_expect <dockerfile> <target> <want_stdout>
  local out
  if ! out=$("$script" "$1" "$2"); then
    echo "FAIL repo $1 $2: exited non-zero"; failures=$((failures + 1))
  elif [ "$out" != "$3" ]; then
    echo "FAIL repo $1 $2: got [$out], want [$3]"; failures=$((failures + 1))
  fi
}
for f in Dockerfile.unified Dockerfile.package; do
  for t in claude codex commandcode copilot gemini mimocode opencode pi; do repo_expect "$f" "$t" NODE22_VERSION; done
  repo_expect "$f" kimi NODE24_VERSION
  for t in kiro cursor grok devin antigravity native agentcore; do repo_expect "$f" "$t" ''; done
done
echo "ok   repository targets"

[ "$failures" -eq 0 ] || { echo "$failures test(s) failed"; exit 1; }
echo "all node_base_arg.sh tests passed"
