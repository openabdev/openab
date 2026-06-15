{{- define "openab-line.fullname" -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "openab-line.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 }}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "openab-line.selectorLabels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "openab-line.agentImage" -}}
{{- $tag := .Values.image.tag -}}
{{- if not $tag -}}
  {{- $tag = .Values.channel | default "stable" -}}
{{- end -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end }}

{{- define "openab-line.gatewayImage" -}}
{{- printf "%s:%s" .Values.gateway.image .Values.gateway.tag -}}
{{- end }}

{{- define "openab-line.secretName" -}}
{{- .Values.existingSecret | default (include "openab-line.fullname" .) -}}
{{- end }}

{{- define "openab-line.tunnelEnabled" -}}
{{- if and .Values.tunnel (kindIs "bool" .Values.tunnel.enabled) .Values.tunnel.enabled -}}
true
{{- end -}}
{{- end }}
