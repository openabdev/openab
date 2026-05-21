{{- define "openab-feishu.fullname" -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "openab-feishu.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 }}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "openab-feishu.selectorLabels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "openab-feishu.agentImage" -}}
{{- $tag := .Values.image.tag -}}
{{- if not $tag -}}
  {{- $tag = .Values.channel | default "stable" -}}
{{- end -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end }}

{{- define "openab-feishu.gatewayImage" -}}
{{- printf "%s:%s" .Values.gateway.image .Values.gateway.tag -}}
{{- end }}

{{- define "openab-feishu.secretName" -}}
{{- .Values.existingSecret | default (include "openab-feishu.fullname" .) -}}
{{- end }}

{{- define "openab-feishu.tunnelEnabled" -}}
{{- if .Values.tunnel.enabled -}}
true
{{- else if and (eq .Values.feishu.connectionMode "webhook") .Values.tunnel.token -}}
true
{{- else -}}
{{- end -}}
{{- end }}
