{{/*
Expand the name of the chart.
*/}}
{{- define "postcards.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "postcards.fullname" -}}
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

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "postcards.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "postcards.labels" -}}
helm.sh/chart: {{ include "postcards.chart" . }}
{{ include "postcards.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "postcards.selectorLabels" -}}
app.kubernetes.io/name: {{ include "postcards.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "postcards.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "postcards.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Immich secret name
*/}}
{{- define "postcards.immichSecretName" -}}
{{- if .Values.immich.existingSecret }}
{{- .Values.immich.existingSecret }}
{{- else }}
{{- printf "%s-immich" (include "postcards.fullname" .) }}
{{- end }}
{{- end }}

{{/*
PCC secret name
*/}}
{{- define "postcards.pccSecretName" -}}
{{- if .Values.pcc.existingSecret }}
{{- .Values.pcc.existingSecret }}
{{- else }}
{{- printf "%s-pcc" (include "postcards.fullname" .) }}
{{- end }}
{{- end }}

{{/*
Initial Token secret name
*/}}
{{- define "postcards.tokenSecretName" -}}
{{- if .Values.token.existingSecret }}
{{- .Values.token.existingSecret }}
{{- else }}
{{- printf "%s-token" (include "postcards.fullname" .) }}
{{- end }}
{{- end }}

{{/*
PersistentVolumeClaim name
*/}}
{{- define "postcards.pvcName" -}}
{{- if .Values.persistence.existingClaim }}
{{- .Values.persistence.existingClaim }}
{{- else }}
{{- printf "%s-data" (include "postcards.fullname" .) }}
{{- end }}
{{- end }}
