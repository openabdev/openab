// docker-bake.hcl — DRY build definitions for all OpenAB images.
// Shared Rust build stage is defined once in Dockerfile.builder;
// each variant Dockerfile references it via multi-stage FROM.
//
// Usage:
//   docker buildx bake <target>       # build a single variant
//   docker buildx bake                # build all variants
//
// CI still uses plain `docker build -f Dockerfile.X .` which continues
// to work because each Dockerfile is self-contained (they inline the
// builder stage via the common 12-line pattern from Dockerfile.builder).
//
// This bake file enables LOCAL development to share a single cached
// builder layer across all variants, dramatically speeding up rebuilds.

group "default" {
  targets = [
    "kiro", "claude", "codex", "copilot", "cursor",
    "devin", "gemini", "grok", "hermes", "kimi", "mimocode",
    "opencode", "antigravity", "pi", "gateway",
  ]
}

// --- Shared builder target (cached across all variants) ---
target "builder" {
  dockerfile = "Dockerfile.builder"
  target     = "builder"
}

// --- Variant targets ---
target "kiro" {
  dockerfile = "Dockerfile"
  tags       = ["openab:kiro"]
  contexts   = { builder = "target:builder" }
}

target "kiro-unified" {
  dockerfile = "Dockerfile"
  tags       = ["openab:kiro-unified"]
  args       = { BUILD_MODE = "unified" }
  contexts   = { builder = "target:builder" }
}

target "claude" {
  dockerfile = "Dockerfile.claude"
  tags       = ["openab:claude"]
  contexts   = { builder = "target:builder" }
}

target "codex" {
  dockerfile = "Dockerfile.codex"
  tags       = ["openab:codex"]
  contexts   = { builder = "target:builder" }
}

target "copilot" {
  dockerfile = "Dockerfile.copilot"
  tags       = ["openab:copilot"]
  contexts   = { builder = "target:builder" }
}

target "cursor" {
  dockerfile = "Dockerfile.cursor"
  tags       = ["openab:cursor"]
  contexts   = { builder = "target:builder" }
}

target "devin" {
  dockerfile = "Dockerfile.devin"
  tags       = ["openab:devin"]
  contexts   = { builder = "target:builder" }
}

target "gemini" {
  dockerfile = "Dockerfile.gemini"
  tags       = ["openab:gemini"]
  contexts   = { builder = "target:builder" }
}

target "grok" {
  dockerfile = "Dockerfile.grok"
  tags       = ["openab:grok"]
  contexts   = { builder = "target:builder" }
}

target "hermes" {
  dockerfile = "Dockerfile.hermes"
  tags       = ["openab:hermes"]
  contexts   = { builder = "target:builder" }
}

target "kimi" {
  dockerfile = "Dockerfile.kimi"
  tags       = ["openab:kimi"]
  contexts   = { builder = "target:builder" }
}

target "mimocode" {
  dockerfile = "Dockerfile.mimocode"
  tags       = ["openab:mimocode"]
  contexts   = { builder = "target:builder" }
}

target "opencode" {
  dockerfile = "Dockerfile.opencode"
  tags       = ["openab:opencode"]
  contexts   = { builder = "target:builder" }
}

target "antigravity" {
  dockerfile = "Dockerfile.antigravity"
  tags       = ["openab:antigravity"]
  contexts   = { builder = "target:builder" }
}

target "pi" {
  dockerfile = "Dockerfile.pi"
  tags       = ["openab:pi"]
  contexts   = { builder = "target:builder" }
}

target "gateway" {
  dockerfile = "Dockerfile.gateway"
  tags       = ["openab:gateway"]
  contexts   = { builder = "target:builder" }
}
