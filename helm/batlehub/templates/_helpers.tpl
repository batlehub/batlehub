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

Helm loads all YAML numbers as float64 internally, so toToml would emit
"8080.0" for port — which toml 0.8 / serde reject for integer fields.
This helper coerces all known integer fields back to Go int/int64 before
calling toToml so the TOML output contains bare integers (e.g. 8080).
*/}}
{{- define "batlehub.config" -}}
{{- $c := .Values.config | deepCopy -}}
{{- /* server */ -}}
{{- if $c.server -}}
  {{- $_ := set $c.server "port" (int $c.server.port) -}}
{{- end -}}
{{- /* database */ -}}
{{- if $c.database -}}
  {{- if hasKey $c.database "max_connections" -}}
    {{- $_ := set $c.database "max_connections" (int (index $c.database "max_connections")) -}}
  {{- end -}}
{{- end -}}
{{- /* limits */ -}}
{{- if $c.limits -}}
  {{- if hasKey $c.limits "max_artifact_size_bytes" -}}
    {{- $_ := set $c.limits "max_artifact_size_bytes" (int64 (index $c.limits "max_artifact_size_bytes")) -}}
  {{- end -}}
{{- end -}}
{{- /* ip_blocking */ -}}
{{- if $c.ip_blocking -}}
  {{- range $f := list "violation_threshold" "violation_window_secs" "ban_duration_secs" -}}
    {{- if hasKey $c.ip_blocking $f -}}
      {{- $_ := set $c.ip_blocking $f (int64 (index $c.ip_blocking $f)) -}}
    {{- end -}}
  {{- end -}}
  {{- if hasKey $c.ip_blocking "trigger_on_status" -}}
    {{- $converted := list -}}
    {{- range $s := index $c.ip_blocking "trigger_on_status" -}}
      {{- $converted = append $converted (int $s) -}}
    {{- end -}}
    {{- $_ := set $c.ip_blocking "trigger_on_status" $converted -}}
  {{- end -}}
{{- end -}}
{{- /* per-registry integer fields */ -}}
{{- range $reg := $c.registries -}}
  {{- if $reg.cache -}}
    {{- range $f := list "metadata_ttl_secs" "artifact_ttl_secs" "idle_days" "max_size_bytes" "keep_latest_n" "warm_latest_n" "warm_concurrency" -}}
      {{- if hasKey $reg.cache $f -}}
        {{- $_ := set $reg.cache $f (int64 (index $reg.cache $f)) -}}
      {{- end -}}
    {{- end -}}
  {{- end -}}
  {{- if $reg.rate_limit -}}
    {{- range $f := list "requests_per_window" "window_secs" -}}
      {{- if hasKey $reg.rate_limit $f -}}
        {{- $_ := set $reg.rate_limit $f (int (index $reg.rate_limit $f)) -}}
      {{- end -}}
    {{- end -}}
  {{- end -}}
  {{- if $reg.quota -}}
    {{- range $f := list "max_storage_bytes_per_user" "max_packages_per_user" -}}
      {{- if hasKey $reg.quota $f -}}
        {{- $_ := set $reg.quota $f (int64 (index $reg.quota $f)) -}}
      {{- end -}}
    {{- end -}}
  {{- end -}}
  {{- range $rule := $reg.rules -}}
    {{- if hasKey $rule "min_age_secs" -}}
      {{- $_ := set $rule "min_age_secs" (int64 (index $rule "min_age_secs")) -}}
    {{- end -}}
  {{- end -}}
{{- end -}}
{{- $c | toToml -}}
{{- end }}

{{/*
  The credentials layer, as TOML.

  `credentials.config` has the same shape as `config` and is serialised the same
  way, so a section written in one is written the same in the other. It runs
  through the same integer coercions because `toToml` renders an unmarked Helm
  number as a float, and `port = 8080.0` is not a port.
*/}}
{{- define "batlehub.credentialsConfig" -}}
{{- $c := .Values.credentials.config | deepCopy -}}
{{- if $c.server -}}
  {{- if hasKey $c.server "port" -}}
    {{- $_ := set $c.server "port" (int $c.server.port) -}}
  {{- end -}}
{{- end -}}
{{- if $c.database -}}
  {{- if hasKey $c.database "max_connections" -}}
    {{- $_ := set $c.database "max_connections" (int (index $c.database "max_connections")) -}}
  {{- end -}}
{{- end -}}
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
