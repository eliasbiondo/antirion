{{- define "antirion.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "antirion.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "antirion.labels" -}}
app.kubernetes.io/name: {{ include "antirion.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
{{- end -}}

{{- define "antirion.selector" -}}
app.kubernetes.io/name: {{ include "antirion.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: gateway
{{- end -}}

{{- define "antirion.cluster" -}}
{{- if eq .Values.mode "cluster" -}}true{{- end -}}
{{- end -}}

{{- define "antirion.secretName" -}}
{{- default (printf "%s-keys" (include "antirion.fullname" .)) .Values.secrets.existingSecret -}}
{{- end -}}

{{- define "antirion.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "antirion.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/* drain_max + flush_timeout + 5 s: the drain and the flush always end before the pod is killed. */}}
{{- define "antirion.gracePeriod" -}}
{{- add (int .Values.shutdown.drainMaxSeconds) (int .Values.shutdown.flushTimeoutSeconds) 5 -}}
{{- end -}}

{{- define "antirion.config" -}}
{{- $cluster := include "antirion.cluster" . -}}
{{- $base := dict
  "version" 1
  "server" (dict
    "listen" (printf "0.0.0.0:%d" (int .Values.service.dataPort))
    "shutdown" (dict
      "drain_min" (printf "%ds" (int .Values.shutdown.drainMinSeconds))
      "drain_max" (printf "%ds" (int .Values.shutdown.drainMaxSeconds))
      "flush_timeout" (printf "%ds" (int .Values.shutdown.flushTimeoutSeconds))))
  "admin" (dict "listen" (printf "0.0.0.0:%d" (int .Values.service.adminPort)))
  "auth" (dict "admin" (dict "bootstrap_token" "${env:ANTIRION_BOOTSTRAP_TOKEN}"))
  "security" (dict "secrets" (dict "encryption_keys" (list "${env:ANTIRION_ENCRYPTION_KEY}")))
-}}
{{- if $cluster -}}
{{- $_ := set $base "store" (dict "backend" "postgres" "postgres" (dict "url" "${env:ANTIRION_POSTGRES_URL}")) -}}
{{- $topology := ternary "standalone" .Values.valkey.external.topology .Values.valkey.bundled.enabled -}}
{{- $_ := set $base "cluster" (dict "mode" "cluster" "counters" (dict "backend" "redis" "redis" (dict "url" "${env:ANTIRION_VALKEY_URL}" "topology" $topology))) -}}
{{- else -}}
{{- $_ := set $base "store" (dict "backend" "embedded" "embedded" (dict "path" "/var/lib/antirion/data") "spool" (dict "path" "/var/lib/antirion/data/spool")) -}}
{{- $_ := set $base "cluster" (dict "mode" "standalone") -}}
{{- end -}}
{{- with .Values.console.publicOrigin -}}
{{- $_ := set $base.auth.admin "public_origin" . -}}
{{- end -}}
{{- with .Values.console.dataPlaneUrl -}}
{{- $_ := set $base.admin "ui" (dict "data_plane_url" .) -}}
{{- end -}}
{{- toYaml (mustMergeOverwrite $base (deepCopy .Values.config)) -}}
{{- end -}}
