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

{{/* Resolve image: agent-level string override → global default (repository:tag, tag defaults to appVersion).
    Caveat: "contains :" treats registry ports (e.g. my-registry:5000/img) as tagged.
    Not an issue for ghcr.io / Docker Hub; revisit if custom registries with ports are needed. */}}
{{- define "openab.agentImage" -}}
{{- if and .cfg.image (kindIs "string" .cfg.image) (ne .cfg.image "") }}
{{- if contains ":" .cfg.image }}
{{- .cfg.image }}
{{- else }}
{{- printf "%s:%s" .cfg.image (default .ctx.Chart.AppVersion .ctx.Values.image.tag) }}
{{- end }}
{{- else }}
{{- $tag := default .ctx.Chart.AppVersion .ctx.Values.image.tag }}
{{- printf "%s:%s" .ctx.Values.image.repository $tag }}
{{- end }}
{{- end }}

{{/* Resolve imagePullPolicy: global default (per-agent image string has no pullPolicy) */}}
{{- define "openab.agentImagePullPolicy" -}}
{{- .ctx.Values.image.pullPolicy }}
{{- end }}

{{/* Agent enabled: default true unless explicitly set to false */}}
{{- define "openab.agentEnabled" -}}
{{- if eq (.enabled | toString) "false" }}false{{ else }}true{{ end }}
{{- end }}

{{/* Persistence enabled: default true unless explicitly set to false */}}
{{- define "openab.persistenceEnabled" -}}
{{- if and . .persistence (eq (.persistence.enabled | toString) "false") }}false{{ else }}true{{ end }}
{{- end }}
