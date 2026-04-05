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
Validate activeAgent and return the agent config dict.
Fails with a clear error if activeAgent is not found in .Values.agents.
*/}}
{{- define "agent-broker.activeAgent" -}}
{{- $agent := index .Values.agents .Values.activeAgent -}}
{{- if not $agent -}}
  {{- fail (printf "activeAgent '%s' not found in .Values.agents — valid options: %s" .Values.activeAgent (keys .Values.agents | sortAlpha | join ", ")) -}}
{{- end -}}
{{- $agent | toJson -}}
{{- end }}

{{/*
Resolve active agent image repository
*/}}
{{- define "agent-broker.image.repository" -}}
{{- $agent := include "agent-broker.activeAgent" . | fromJson -}}
{{- $agent.image.repository -}}
{{- end }}

{{/*
Resolve active agent image tag (falls back to .Chart.AppVersion)
*/}}
{{- define "agent-broker.image.tag" -}}
{{- $agent := include "agent-broker.activeAgent" . | fromJson -}}
{{- $agent.image.tag | default .Chart.AppVersion -}}
{{- end }}

{{/*
Resolve active agent command
*/}}
{{- define "agent-broker.agent.command" -}}
{{- $agent := include "agent-broker.activeAgent" . | fromJson -}}
{{- $agent.command -}}
{{- end }}

{{/*
Resolve active agent args as JSON array
*/}}
{{- define "agent-broker.agent.args" -}}
{{- $agent := include "agent-broker.activeAgent" . | fromJson -}}
{{- $agent.args | toJson -}}
{{- end }}
