---
sourcePath: operations/production-hardening.md
sourceHash: abfdcda6d4739e0b
---

# Liste de contrôle du durcissement en production

Les réglages **délibérément permissifs ou désactivés par défaut**, à revoir avant
qu'un déploiement ne soit exposé à du trafic réel. Rien ici n'est un bug ; chaque
défaut est choisi pour qu'un premier lancement fonctionne sans configuration. La
production est l'endroit où l'on échange cette commodité contre autre chose.

Pour le volet disponibilité (réplicas, backend de cache partagé, mises à jour
progressives), voir [Haute disponibilité](/fr/guide/high-availability). Pour
l'analyse automatique du code lui-même, voir
[Vulnerability scanning](/contributing/security-scanning) (en anglais).

---

## 1. La confiance envers le reverse proxy — `[server].trusted_proxies`

**Défaut : non défini, c'est-à-dire que les en-têtes transmis sont crus de
n'importe quel pair.**

`Forwarded`, `X-Forwarded-Host` et `X-Forwarded-Proto` décident de l'hôte dans
toute URL que BatleHub génère, et `X-Forwarded-For` décide de l'IP cliente à
laquelle le blocage par IP impute les violations. Sans liste configurée, les
trois premiers sont crus sans condition et le quatrième est entièrement ignoré.

Réglez-le sur le CIDR de ce qui termine TLS devant vous :

```toml
[server]
trusted_proxies = ["10.42.0.0/16"]
```

Le routage par hôte rend ce réglage obligatoire — le serveur refuse de démarrer
avec `subdomain_routing` ou une entrée `hosts` de registre et aucune politique de
confiance, parce que le routage serait sinon décidé par un en-tête que n'importe
quel client peut poser.

## 2. CORS — `[server].cors_allowed_origins`

**Défaut (depuis la 1.1.0) : même origine uniquement.** Rien à faire, sauf si la
console est servie depuis une origine différente de l'API, auquel cas nommez
cette origine explicitement. Évitez `["*"]` ; cette valeur lève un avertissement
de configuration `cors.any-origin`, précisément pour ne pas passer inaperçue.

## 3. La limitation de débit — `[registries.rate_limit]`

**Défaut : désactivée.** Configurez-la registre par registre, et notez le
magasin : le seau à jetons en mémoire est propre à chaque processus, donc avec
plus d'un réplica, chacun dispose de la totalité de l'allocation. Employez le
magasin Redis ou Postgres pour tout déploiement multi-réplicas.

```toml
[registries.rate_limit]
requests_per_window = 600
window_secs         = 60
enforcement         = "block"
```

## 4. Le blocage par IP — `[ip_blocking]`

**Défaut : `enabled = false`.** Activez-le pour un déploiement exposé à Internet.
Il dépend de la justesse de l'IP cliente : il n'a donc de sens qu'une fois le §1
réglé — sinon un client peut nommer l'adresse qu'il veut voir bloquer.

## 5. `/metrics` n'est pas authentifié

C'est voulu, pour qu'un collecteur Prometheus n'ait besoin d'aucun identifiant.
L'endpoint expose les noms de registres, la cardinalité des paquets et des
compteurs d'erreurs. Restreignez-le à l'ingress plutôt que de le publier :

```yaml
# ingress nginx : n'exposer /metrics qu'au namespace de supervision
nginx.ingress.kubernetes.io/server-snippet: |
  location /metrics { deny all; }
```

Collectez-le plutôt depuis l'intérieur du cluster, par le Service.

## 6. HSTS à l'ingress

BatleHub envoie `X-Content-Type-Options`, `X-Frame-Options` et
`Referrer-Policy` sur toutes ses réponses, et
`Content-Security-Policy: default-src 'none'; sandbox` sur tout ce qui est sous
`/proxy/**` — les documents de protocole et les octets d'artefact, c'est-à-dire
les réponses qu'un éditeur peut influencer et qu'un navigateur peut rendre. La
console n'est pas sous ce préfixe et garde sa propre politique ; `nosniff` seul
n'a jamais couvert ce cas, parce qu'un document comme l'index Simple de PyPI
déclare honnêtement `text/html`.

Délibérément **non** envoyé : `Strict-Transport-Security` — TLS se termine à
l'ingress et le serveur lui-même parle en général du HTTP en clair, donc émettre
HSTS depuis derrière le proxy risquerait d'épingler les navigateurs sur `https://`
pour un hôte incapable de le servir. Posez-le là où TLS se termine :

```yaml
ingress:
  annotations:
    nginx.ingress.kubernetes.io/configuration-snippet: |
      more_set_headers "Strict-Transport-Security: max-age=31536000; includeSubDomains";
```

## 7. Les secrets

Aucun identifiant ne doit être un littéral dans `values.yaml` ou `config.toml`.
Employez des marqueurs `${VAR}` développés depuis des variables d'environnement
issues d'un Secret — BatleHub s'arrête au démarrage en nommant tout marqueur dont
la variable manque, de sorte qu'une faute de frappe échoue bruyamment plutôt que
de s'authentifier silencieusement sous une identité vide.

Avant la mise en service, cherchez dans la configuration rendue les marqueurs
livrés dans les exemples : `change-me-admin-token`, `change-me-user-token`,
`proxy-auth-secret`, `batlehub-local-insecure-secret-key`. Ils existent pour que
les fichiers d'exemple tournent en local ; aucun ne devrait jamais atteindre un
cluster.

## 8. Backend de stockage et nombre de réplicas

`persistence.accessMode: ReadWriteOnce` ne peut pas être partagé par des pods
situés sur des nœuds différents : le stockage sur système de fichiers est donc de
fait mono-réplica. Pour `replicaCount > 1`, employez le stockage S3 — le seul
backend dans lequel tous les réplicas peuvent écrire simultanément — ou une
classe de stockage `ReadWriteMany`.

## 9. La sécurité des pods

Les défauts du chart sont déjà restrictifs depuis la 1.1.0 (`runAsNonRoot`,
uid 65532, `readOnlyRootFilesystem`, toutes les capacités retirées, seccomp
`RuntimeDefault`), et le pod est admissible tel quel dans un namespace en Pod
Security Admission `restricted`. Si vous remplacez `podSecurityContext` ou
`securityContext`, vous remplacez le bloc entier — redéclarez les champs que vous
voulez conserver.

`networkPolicy` est désactivé par défaut, parce que le bon ensemble de sorties
dépend des registres amont dont vous faites proxy. Quand vous l'activez, le chart
émet toujours une règle de sortie DNS en premier ; ajoutez vos amonts sous
`networkPolicy.egressTo`.

---

## Audit rapide

```bash
# Chart rendu : confirmer que les défauts attendus sont bien passés
helm template batlehub ./helm/batlehub -f prod-values.yaml \
  | grep -E 'runAsNonRoot|readOnlyRootFilesystem|allowPrivilegeEscalation|path: /(livez|healthz)'

# Serveur en fonctionnement : les avertissements de configuration nomment ce qui reste permissif
curl -sH "Authorization: Bearer $ADMIN_TOKEN" \
  https://batlehub.example.com/api/v1/admin/config/warnings | jq
```

`GET /api/v1/admin/config/warnings` est le contrôle le plus rapide, mais il ne
signale que les problèmes **non fatals** — le serveur doit tourner pour y
répondre. Chaque avertissement porte un `code` stable et le `path` de la clé
fautive. Une origine CORS générique (`cors.any-origin`) y apparaît, tout comme
une politique de confiance envers les proxys non énoncée
(`proxy-trust.unconfigured`) sur un déploiement sans routage par hôte.

Les échecs durs de validation, eux, n'atteignent jamais cet endpoint. Combiner le
routage par hôte et l'absence de `trusted_proxies` en est un : le routage serait
décidé par un en-tête sans politique sur qui peut le poser, donc
`AppConfig::validate` refuse de démarrer et imprime la raison. Pour ceux-là, lisez
le log de démarrage.
