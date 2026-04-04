{{- define "agent-broker.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "agent-broker.fullname" -}}
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

{{- define "agent-broker.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "agent-broker.labels" -}}
helm.sh/chart: {{ include "agent-broker.chart" . }}
{{ include "agent-broker.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "agent-broker.selectorLabels" -}}
app.kubernetes.io/name: {{ include "agent-broker.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Resolve agent preset → image repository
*/}}
{{- define "agent-broker.image.repository" -}}
{{- if .Values.agent.preset }}
  {{- if eq .Values.agent.preset "codex" }}ghcr.io/thepagent/agent-broker-codex
  {{- else if eq .Values.agent.preset "claude" }}ghcr.io/thepagent/agent-broker-claude
  {{- else if eq .Values.agent.preset "gemini" }}ghcr.io/thepagent/agent-broker-gemini
  {{- else }}{{ .Values.image.repository }}
  {{- end }}
{{- else }}{{ .Values.image.repository }}
{{- end }}
{{- end }}

{{/*
Resolve agent preset → command
*/}}
{{- define "agent-broker.agent.command" -}}
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
{{- define "agent-broker.agent.args" -}}
{{- if .Values.agent.preset }}
  {{- if or (eq .Values.agent.preset "codex") (eq .Values.agent.preset "claude") }}[]
  {{- else if eq .Values.agent.preset "gemini" }}["--acp"]
  {{- else }}{{ .Values.agent.args | toJson }}
  {{- end }}
{{- else }}{{ .Values.agent.args | toJson }}
{{- end }}
{{- end }}
