{{- define "openab.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "openab.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "openab.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "openab.labels" -}}
helm.sh/chart: {{ include "openab.chart" .ctx }}
app.kubernetes.io/name: {{ include "openab.name" .ctx }}
app.kubernetes.io/instance: {{ .ctx.Release.Name }}
app.kubernetes.io/component: {{ .agent }}
{{- if .ctx.Chart.AppVersion }}
app.kubernetes.io/version: {{ .ctx.Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .ctx.Release.Service }}
{{- end }}

{{- define "openab.selectorLabels" -}}
app.kubernetes.io/name: {{ include "openab.name" .ctx }}
app.kubernetes.io/instance: {{ .ctx.Release.Name }}
app.kubernetes.io/component: {{ .agent }}
{{- end }}

{{/* Per-agent resource name: <fullname>-<agentKey> */}}
{{- define "openab.agentFullname" -}}
{{- printf "%s-%s" (include "openab.fullname" .ctx) .agent | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Resolve image: agent-level string override → global default (repository:tag, tag defaults to appVersion) */}}
{{- define "openab.agentImage" -}}
{{- if and .cfg.image (kindIs "string" .cfg.image) (ne .cfg.image "") }}
{{- .cfg.image }}
{{- else }}
{{- $tag := default .ctx.Chart.AppVersion .ctx.Values.image.tag }}
{{- printf "%s:%s" .ctx.Values.image.repository $tag }}
{{- end }}
{{- end }}

{{/* Resolve imagePullPolicy: per-agent override or global default */}}
{{- define "openab.agentImagePullPolicy" -}}
{{- default .ctx.Values.image.pullPolicy .cfg.imagePullPolicy }}
{{- end }}

{{/* Resolve imagePullSecrets: per-agent override (if explicitly set, including empty list) or global default */}}
{{- define "openab.agentImagePullSecrets" -}}
{{- $pullSecrets := .ctx.Values.imagePullSecrets -}}
{{- if hasKey .cfg "imagePullSecrets" -}}
{{- $pullSecrets = .cfg.imagePullSecrets -}}
{{- end }}
{{- range $pullSecrets }}
- name: {{ . | quote }}
{{- end }}
{{- end }}

{{/* Resolve serviceAccountName: per-agent only, empty by default (uses namespace default SA) */}}
{{- define "openab.agentServiceAccountName" -}}
{{- default "" .cfg.serviceAccountName }}
{{- end }}

{{/*
Pod annotations: global baseline + per-agent override, with reserved
chart-managed annotations (checksum/config) merged last so users cannot
clobber them and produce duplicate YAML keys.
*/}}
{{- define "openab.agentPodAnnotations" -}}
{{- $reserved := dict "checksum/config" (.cfg | toJson | sha256sum) -}}
{{- $annotations := mergeOverwrite (dict)
    (.ctx.Values.podAnnotations | default (dict))
    (.cfg.podAnnotations | default (dict))
    $reserved -}}
{{- toYaml $annotations }}
{{- end }}

{{/*
Pod labels: global baseline + per-agent override, with reserved selector
labels merged last so users cannot hijack them. Hijacking would produce
duplicate YAML keys AND break Deployment→Pod selector matching.
*/}}
{{- define "openab.agentPodLabels" -}}
{{- $reserved := dict
    "app.kubernetes.io/name" (include "openab.name" .ctx)
    "app.kubernetes.io/instance" .ctx.Release.Name
    "app.kubernetes.io/component" .agent
-}}
{{- $labels := mergeOverwrite (dict)
    (.ctx.Values.podLabels | default (dict))
    (.cfg.podLabels | default (dict))
    $reserved -}}
{{- toYaml $labels }}
{{- end }}

{{/* Agent enabled: default true unless explicitly set to false */}}
{{- define "openab.agentEnabled" -}}
{{- if eq (.enabled | toString) "false" }}false{{ else }}true{{ end }}
{{- end }}

{{/* Persistence enabled: default true unless explicitly set to false */}}
{{- define "openab.persistenceEnabled" -}}
{{- if and . .persistence (eq (.persistence.enabled | toString) "false") }}false{{ else }}true{{ end }}
{{- end }}
