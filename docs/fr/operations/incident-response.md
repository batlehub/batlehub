---
sourcePath: operations/incident-response.md
sourceHash: a44f5e9b2acde705
---

# Procédure de réponse à incident — BatleHub

Cette procédure couvre les incidents de sécurité et de disponibilité d'un
déploiement BatleHub. Suivez les phases dans l'ordre.

::: warning Des repères, pas un engagement
C'est un modèle pour la procédure que *vous* écrivez, pas un service que le
projet assure. Les niveaux de gravité, les temps de réponse et les destinataires
des notifications ci-dessous sont des exemples — remplacez-les par ceux de votre
organisation, parce que les seules personnes capables de les tenir sont les
vôtres. Personne n'est appelé par cette page.
:::

---

## Niveaux de gravité

| Niveau | Définition | Délai de réponse |
|-------|-----------|-------------|
| **P0 — critique** | Fuite de données, compromission de token, registre indisponible plus de 15 min | Réponse immédiate (24 h/24) |
| **P1 — élevé** | Accès non autorisé aux endpoints d'administration, déni de service persistant | Sous 2 heures |
| **P2 — moyen** | Motifs d'accès anormaux, attaque échouée mais détectée | Sous 8 heures |
| **P3 — faible** | Politique mal configurée, alertes de tokens expirés | Le jour ouvré suivant |

---

## Phase 1 — détection

### Signaux automatiques

| Source | Alerte | Où regarder |
|--------|-------|----------------|
| Prometheus | `BatleHubDown`, `BatleHubHighErrorRate`, `BatleHubHighDenyRate` | `deploy/prometheus-alerts.yaml` |
| Journal d'audit | Pic d'issues `denied`, identifiants inconnus dans la colonne `user_id` | `GET /api/v1/admin/audit-log` |
| Limiteur de débit | 429 soutenus depuis une même IP | Filtre du journal d'audit par IP |
| Analyse de conteneur | Constat HIGH ou CRITICAL de Trivy sur l'image déployée | `.github/workflows/image-scan.yaml` |
| Gitleaks | Secret exposé dans un commit | `.github/workflows/secret-scan.yaml` |

### Détection manuelle

Interrogez le journal d'audit à la recherche d'anomalies :

```bash
# Tous les événements refusés de la dernière heure
batlehub admin audit-log --denied-only --from "$(date -u -d '1 hour ago' +%FT%TZ)"

# Les événements d'accès d'une IP précise
batlehub admin audit-log | jq '[.[] | select(.ip_address == "1.2.3.4")]'

# Ce qui a disparu, et si c'est une personne ou une politique qui l'a pris
batlehub admin audit-log --action delete,retention_reclaim --from <start>

# Exporter une fenêtre complète de 24 heures pour analyse hors ligne
batlehub admin export-audit-log --from <start> --to <end> --format csv --output incident-$(date +%Y%m%d).csv
```

---

## Phase 2 — confinement

Agissez dans les 15 premières minutes pour un P0 ou un P1. N'attendez pas
l'analyse des causes racines pour confiner.

### Bloquer une IP suspecte

```bash
batlehub admin ip-blocks add 1.2.3.4
```

Ou depuis la console d'administration → **Blocages d'IP** → Ajouter.

### Révoquer un token compromis

```bash
batlehub admin token revoke <token-id>
```

Ou depuis la console → **Utilisateurs** → sélectionner l'utilisateur → Révoquer
le token.

### Bloquer un compte utilisateur

```bash
batlehub admin users block <user-id>
```

Bloque toutes les requêtes futures de cet identifiant jusqu'au déblocage.

### Mettre un paquet malveillant en quarantaine

```bash
# Bloquer le nom du paquet, toutes versions confondues
batlehub admin block <registry> <package-name>

# Ou viser une version précise
batlehub admin packages unlist <registry> <package-name> <version>
```

### Signaler une version depuis l'outillage de votre SOC

Une entrée `[[flag_sources]]`
([configuration §3.11](/fr/guide/configuration#flag-sources)) permet à la
plateforme du SOC de pousser elle-même le verdict, signé, sans connexion à la
console — et cette poussée est l'enregistrement que lit le rapport d'exposition.

```bash
BODY='{"flags":[{"external_id":"CASE-2026-0912","registry":"npm","package_name":"left-pad",
  "version":"1.3.1","kind":"malware","effect":"hard_block","summary":"credential stealer in postinstall"}]}'
SIG="sha256=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$FLAG_SECRET" | sed 's/^.* //')"
curl -sS -X POST "$BATLEHUB/api/v1/flags/soc" \
  -H "Content-Type: application/json" -H "X-Hub-Signature-256: $SIG" --data "$BODY"
```

`version_range = "*"` signale toutes les versions du paquet. Sur un registre
doté d'un profil `[registries.security]`, la version est refusée aussitôt ;
ailleurs, `FlagsRule` la refuse à la requête suivante. Levez le signalement avec
`DELETE /api/v1/flags/soc/CASE-2026-0912` (signé sur
`DELETE\n/api/v1/flags/{source}/{external_id}`, de sorte que la signature est liée
au signalement qu'elle lève) : la ligne reste en pierre tombale, pour que le
rapport ci-dessous réponde encore.

### Qui a récupéré une version signalée {#who-pulled-a-flagged-version}

Le rapport d'exposition joint le journal d'accès aux signalements, une ligne par
consommateur, coordonnée et signalement, et dit combien de récupérations ont
**précédé** le signalement — le cas rétroactif, où le développeur n'a rien fait
de mal et détient tout de même l'artefact.

```bash
batlehub admin exposure --source soc --min-effect gate --when before-flag
batlehub admin exposure --registry npm --package left-pad --from 2026-09-01T00:00:00Z
batlehub admin flags list --include-dead          # ce que chaque source a dit, pierres tombales comprises
```

`GET /api/v1/admin/exposure/export?format=csv` donne les mêmes lignes sous forme
de fichier, pour le ticket. Lisez le bloc de **couverture** avant de citer les
chiffres : il dit combien de registres ont un extracteur de SBOM (donc jusqu'où
l'analyse CVE porte), quand cette analyse a tourné pour la dernière fois par
registre, et quelles sources ont poussé quelque chose — un rapport qui n'a jamais
analysé ne connaît que ce qui a été poussé.

### Isoler un réplica

Si un réplica est compromis, retirez-le du répartiteur de charge avant toute
analyse forensique. L'état de BatleHub vit dans Postgres et sur S3 — le réplica
lui-même est sans état.

---

## Phase 3 — éradication

1. **Faites tourner les identifiants** — générez de nouveaux tokens d'API pour
   tous les comptes de service ; mettez à jour les consommateurs en aval.
2. **Corrigez la vulnérabilité** — si une CVE a déclenché l'incident, appliquez
   le correctif, lancez `cargo audit` en local, et reconstruisez l'image.
3. **Relancez l'analyse SBOM** — `task security`, pour confirmer que l'arbre de
   dépendances corrigé est propre.
4. **Revoyez les règles RBAC** — servez-vous du simulateur RBAC
   (`POST /api/v1/admin/access-check`) pour valider que le trou de politique
   concerné est fermé.

---

## Phase 4 — reprise

1. **Déployez l'image corrigée** — lancez `cargo build --release` ou déclenchez
   la CI ; poussez le conteneur corrigé.
2. **Vérifiez la santé** — `GET /healthz` répond `200` sur tous les réplicas. Cet
   endpoint est exempté d'authentification, ce qui le rend utilisable depuis une
   sonde ou un script sans identifiant ;
   `GET /api/v1/admin/health` est la vue détaillée par registre et exige un token
   d'admin.
3. **Débloquez le trafic légitime** — retirez les blocages d'IP et débloquez les
   utilisateurs pris dans les dommages collatéraux.
4. **Surveillez** — observez Prometheus pendant 30 minutes après le
   rétablissement ; confirmez que le taux d'erreur revient à sa valeur de
   référence.

---

## Phase 5 — retour d'expérience

Dans les 5 jours ouvrés suivant un incident P0 ou P1 :

1. Écrivez une chronologie (détection → confinement → éradication → reprise).
2. Identifiez la cause racine et les facteurs contributifs.
3. Listez les actions correctives, avec un responsable et une échéance chacune.
4. Mettez cette procédure à jour si une étape manquait ou n'était pas claire.
5. Archivez le journal d'audit exporté pour la période de l'incident
   (`export-audit-log --format csv`).

Modèle de retour d'expérience : il n'y en a pas encore. Écrivez le premier
contre les cinq points ci-dessus et gardez-le sous `operations/`, pour que
l'incident suivant parte d'un formulaire plutôt que d'une page blanche.

---

## Traitement des données personnelles

Les entrées du journal d'audit contiennent des identifiants d'utilisateur et des
adresses IP. Si une demande de suppression au titre du RGPD ou du CCPA arrive :

1. Identifiez l'identifiant de l'utilisateur depuis son compte.
2. Exportez ses enregistrements :
   `export-audit-log | jq '[.[] | select(.user_id == "X")]'`
3. Fournissez-lui une copie si votre juridiction l'exige.
4. Pour purger la base, exécutez la migration d'anonymisation (fonctionnalité
   prévue) ou un `UPDATE access_events SET user_id = 'anonymized', ip_address =
   NULL WHERE user_id = 'X'` ciblé, sous la supervision d'un DBA.

---

## Contacts

À renseigner avant tout déploiement en production :

| Rôle | Contact |
|------|---------|
| Ingénieur d'astreinte | — |
| Responsable sécurité | — |
| Juridique / DPO | — |
| Contacts chez les registres amont (GitHub, npm, PyPI) | — |
