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
helm.sh/chart: {{ include "openab.chart" . }}
{{ include "openab.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "openab.selectorLabels" -}}
app.kubernetes.io/name: {{ include "openab.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Resolve agent preset → image repository
*/}}
{{- define "openab.image.repository" -}}
{{- if .Values.agent.preset }}
  {{- if eq .Values.agent.preset "codex" }}ghcr.io/openabdev/openab-codex
  {{- else if eq .Values.agent.preset "claude" }}ghcr.io/openabdev/openab-claude
  {{- else if eq .Values.agent.preset "gemini" }}ghcr.io/openabdev/openab-gemini
  {{- else }}{{ .Values.image.repository }}
  {{- end }}
{{- else }}{{ .Values.image.repository }}
{{- end }}
{{- end }}

{{/*
Resolve agent preset → command
*/}}
{{- define "openab.agent.command" -}}
{{- if .Values.agent.preset }}
  {{- if eq .Values.agent.preset "codex" }}codex-acp
  {{- else if eq .Values.agent.preset "claude" }}claude-agent-acp
  {{- else if eq .Values.agent.preset "gemini" }}gemini
  {{- else }}{{ .Values.agent.command }}
  {{- end }}
{{- else }}{{ .Values.agent.command }}
{{- end }}
{{- end }}

{{/*
Resolve agent preset → args
*/}}
{{- define "openab.agent.args" -}}
{{- if .Values.agent.preset }}
  {{- if or (eq .Values.agent.preset "codex") (eq .Values.agent.preset "claude") }}[]
  {{- else if eq .Values.agent.preset "gemini" }}["--acp"]
  {{- else }}{{ .Values.agent.args | toJson }}
  {{- end }}
{{- else }}{{ .Values.agent.args | toJson }}
{{- end }}
{{- end }}

{{/*
Resolve agent preset → home directory
*/}}
{{- define "openab.agent.home" -}}
{{- if and .Values.agent.preset (or (eq .Values.agent.preset "codex") (eq .Values.agent.preset "claude") (eq .Values.agent.preset "gemini")) }}/home/node
{{- else }}/home/agent
{{- end }}
{{- end }}
