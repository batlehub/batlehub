{{/*
Expand the name of the chart.
*/}}
{{- define "batlehub.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "batlehub.fullname" -}}
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
Create chart label.
*/}}
{{- define "batlehub.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels.
*/}}
{{- define "batlehub.labels" -}}
helm.sh/chart: {{ include "batlehub.chart" . }}
{{ include "batlehub.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels.
*/}}
{{- define "batlehub.selectorLabels" -}}
app.kubernetes.io/name: {{ include "batlehub.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
ServiceAccount name.
*/}}
{{- define "batlehub.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "batlehub.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Whether an externalManifest entry (passed as `.`) is enabled. Defaults to
true when the `enabled` key is absent. Implemented with `hasKey` rather than
`default true` because `default` treats an explicit `false` as "unset" and
would coerce it back to true.
*/}}
{{- define "batlehub.manifestEnabled" -}}
{{- $enabled := true -}}
{{- if hasKey . "enabled" -}}
{{- $enabled = .enabled -}}
{{- end -}}
{{- if $enabled }}true{{ else }}false{{ end -}}
{{- end }}

{{/*
Find the (at most one) enabled externalManifest entry with mount.asConfig:
true. Emits it as YAML so callers can do:
  {{- $configEntry := include "batlehub.configManifestEntry" . | fromYaml }}
Emits "{}" (empty dict) when no such entry exists, so $configEntry.name is
always safe to dereference.
*/}}
{{- define "batlehub.configManifestEntry" -}}
{{- $found := dict -}}
{{- range .Values.externalManifest -}}
{{- if and (eq (include "batlehub.manifestEnabled" .) "true") .mount .mount.asConfig -}}
{{- $found = . -}}
{{- end -}}
{{- end -}}
{{- if $found -}}
{{- $found | toYaml -}}
{{- else -}}
{{- "{}" -}}
{{- end -}}
{{- end }}

{{/*
Returns "true" if config.auth contains an entry with type = "kubernetes",
else "".
*/}}
{{- define "batlehub.kubernetesAuthEnabled" -}}
{{- $enabled := false -}}
{{- range .Values.config.auth -}}
{{- if eq (.type | default "") "kubernetes" -}}
{{- $enabled = true -}}
{{- end -}}
{{- end -}}
{{- if $enabled }}true{{ end -}}
{{- end }}

{{/*
Effective automountServiceAccountToken: explicit true, or implied by
Kubernetes TokenReview auth (the provider reads its own projected token to
call the TokenReview API, regardless of who manages the RBAC binding).
*/}}
{{- define "batlehub.automountServiceAccountToken" -}}
{{- if or .Values.serviceAccount.automountServiceAccountToken (eq (include "batlehub.kubernetesAuthEnabled" .) "true") -}}
true
{{- else -}}
false
{{- end -}}
{{- end }}

{{/*
Render config.toml by serializing .Values.config as TOML.
Keys must be snake_case throughout to match batlehub's field names.

Helm loads every YAML number as a float64, so toToml would emit "8080.0" for
port, which serde refuses for an integer field and the server does not start.
`batlehub.wholeFloatsToInts` turns every float with no fractional part back
into an int64 before toToml. That is safe for the real f64 fields too
(`upstream_audit.outage_ratio`): serde reads `1` as `1.0` for an f64, while it
refuses `30.0` for a u32. A float with a fraction is left alone, so `30.5` on
an integer field still reaches the server and is refused there, not truncated.
*/}}
{{- define "batlehub.config" -}}
{{- $c := .Values.config | deepCopy -}}
{{- include "batlehub.wholeFloatsToInts" $c -}}
{{- $c | toToml -}}
{{- end }}

{{/*
  In place, through every nested map and list: a float64 with no fractional
  part becomes an int64. A list is rebuilt and set back on its parent, because
  a template cannot assign to a list element; a nested list goes through a
  one-key wrapper map so it reuses the map case.
*/}}
{{- define "batlehub.wholeFloatsToInts" -}}
{{- $m := . -}}
{{- range $k, $v := $m -}}
  {{- if and (kindIs "float64" $v) (eq $v (floor $v)) -}}
    {{- $_ := set $m $k (int64 $v) -}}
  {{- else if kindIs "map" $v -}}
    {{- include "batlehub.wholeFloatsToInts" $v -}}
  {{- else if kindIs "slice" $v -}}
    {{- $out := list -}}
    {{- range $e := $v -}}
      {{- $w := dict "e" $e -}}
      {{- include "batlehub.wholeFloatsToInts" $w -}}
      {{- $out = append $out $w.e -}}
    {{- end -}}
    {{- $_ := set $m $k $out -}}
  {{- end -}}
{{- end -}}
{{- end }}

{{/*
  The credentials layer, as TOML.

  `credentials.config` has the same shape as `config` and is serialised the same
  way, so a section written in one is written the same in the other — the same
  float-to-int pass included, because `toToml` renders an unmarked Helm number
  as a float, and `port = 8080.0` is not a port.
*/}}
{{- define "batlehub.credentialsConfig" -}}
{{- $c := .Values.credentials.config | deepCopy -}}
{{- include "batlehub.wholeFloatsToInts" $c -}}
{{- toToml $c -}}
{{- end }}

{{/*
  The Secret holding the credentials layer: the one named in values, or the one
  this chart renders. Empty when the layer is off.
*/}}
{{- define "batlehub.credentialsSecretName" -}}
{{- if .Values.credentials.enabled -}}
{{- .Values.credentials.existingSecret | default (printf "%s-credentials" (include "batlehub.fullname" .)) -}}
{{- end -}}
{{- end }}
