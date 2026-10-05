---
sourcePath: operations/siem.md
sourceHash: 0b20be180a3e0845
---

# Intégration SIEM

BatleHub écrit son journal d'audit deux fois : en lignes de la table
`access_events`, que lit le journal d'audit de la console, et — lorsque
`[logging] format = "json"` — en une ligne JSON par enregistrement sur la sortie
standard, qu'un collecteur achemine vers un SIEM. La ligne est émise à l'endroit
même où l'enregistrement est écrit, avec les mêmes champs : une règle de
détection et un export de la table ne peuvent donc jamais diverger sur ce qui
s'est passé. Le dépôt livre des règles Sigma pour ce flux dans
`deploy/siem/sigma/`.

Cette page est la référence du flux et des règles. Son activation tient en un
réglage, documenté dans [configuration § `[logging]`](../guide/configuration.md#logging).

## Activer le flux

```toml
[logging]
format = "json"
```

Chaque ligne de journal devient un objet JSON. Les lignes d'audit sont celles
qui portent `event.dataset = "batlehub.audit"` (de façon équivalente,
`target = "batlehub::audit"`) ; toutes les autres sont des lignes de journal
ordinaires, dans le même format. Il n'y a ni fichier ni rotation : le
collecteur lit la sortie standard.

Le défaut, `text`, écrit exactement ce que ce serveur écrivait avant
l'existence du flux — et aucune ligne d'audit, parce qu'une ligne par
téléchargement est du bruit dans un journal lu par un humain.

## Référence des champs

| Champ | Exemple | Signification |
| --- | --- | --- |
| `event.dataset` | `batlehub.audit` | Le sélecteur. Toute ligne d'audit le porte, aucune autre ligne ne le porte |
| `event.kind` | `event` | Constante ECS |
| `event.category` | `authentication` | `authentication`, `iam`, `configuration` ou `package` — fixé par action |
| `event.action` | `credential_rejected` | L'action, dans le vocabulaire snake_case du journal d'audit |
| `event.outcome` | `denied` | `allowed`, `denied` ou `error` |
| `event.reason` | `blocked: malware flagged by osv (…)` | La raison d'un refus ; vide quand la requête est autorisée |
| `event.id` | UUID | L'identifiant de l'enregistrement dans `access_events` |
| `user.id` | `alice` | Le principal, tel que le nomme le fournisseur qui l'a authentifié ; vide pour un anonyme |
| `user.roles` | `user` | `anonymous`, `user` ou `admin` |
| `source.ip` | `203.0.113.9` | L'appelant, tel que les règles de confiance des proxys l'ont résolu — jamais un `X-Forwarded-For` brut |
| `user_agent.original` | `npm/10.9.0` | Le `User-Agent` du client |
| `batlehub.registry` | `npm` | Le registre, pour les événements de package |
| `package.name`, `package.version` | `left-pad`, `1.3.0` | La coordonnée, pour les événements de package |
| `batlehub.audit.detail` | `token_id=… name="ci"` | Ce sur quoi porte un événement qui ne concerne pas un package — un token, un fournisseur, le sujet d'une grant. Jamais un secret |
| `batlehub.audit.throttled_count` | `20` | Les tentatives qu'un écrivain limité a retenues avant cette ligne (voir plus bas) |
| `batlehub.audit.persisted` | `true` | Si l'écriture en base que reflète cette ligne a réussi |
| `span.request_id` | `7f0c…` | La requête HTTP à laquelle appartient l'événement ; présent sur toutes les lignes de cette requête |

Un champ qui n'a rien à dire est une chaîne vide, pas une clé absente.

`batlehub.audit.persisted = false` signifie que la ligne a été émise et que
l'enregistrement n'a **pas** été stocké — une panne de base de données. Le flux
est alors la seule trace, ce qui suffit à justifier son acheminement.

## Ce qui est enregistré

Toutes les actions que le journal d'audit enregistre passent sur le flux :
téléchargements et listings, blocages, suppressions, grants, exécutions de
rétention et de cache, purges. Six actions concernent l'authentification :

| `event.action` | Quand | `batlehub.audit.detail` |
| --- | --- | --- |
| `sign_in` | Une connexion OIDC a abouti | `provider=<nom>` |
| `sign_in_failed` | Un callback a échoué ; `event.reason` en donne la classe — `state_mismatch`, `token_exchange`, `claims`, `nonce`, … | `provider=<nom>` quand il est connu |
| `token_create` | Un token d'accès personnel a été créé | `token_id=<uuid> name="<nom>"` |
| `token_revoke` | Un token d'accès personnel a été révoqué | `token_id=<uuid>` |
| `token_new_source` | Un token a été accepté depuis une adresse différente de la précédente | `token_id=<uuid> previous_ip=<ip>` |
| `credential_rejected` | Un identifiant a été présenté et aucun fournisseur ne l'a accepté | — |

Aucune ligne ne porte jamais la valeur d'un token, son empreinte, un code
d'autorisation ou un mot de passe.

**`credential_rejected` est limité** : une ligne par IP source et par minute,
par processus serveur. Les tentatives retenues sont comptées, et le compte
voyage sur la ligne *suivante* écrite pour cette IP, dans
`batlehub.audit.throttled_count`. Une rafale de vingt et une tentatives
refusées en une minute donne donc une ligne avec un compte de 0, puis — si les
tentatives continuent — une ligne une minute plus tard avec un compte de 20.
Avec *n* réplicas, chacun écrit ses propres lignes.

**`token_new_source`** est vérifié au moment où la date de dernière
utilisation du token est écrite, au plus une fois par minute et par token. Un
token sans adresse encore enregistrée — sa première utilisation après une mise
à jour — n'est pas « nouveau ».

Les rechargements de configuration rejoignent le flux sous `config_applied` et
`config_rejected`, cette dernière pour un candidat que le watcher de fichier ou
l'éditeur de la console a refusé.

## Les règles livrées

| Règle | Se déclenche sur | Niveau |
| --- | --- | --- |
| `audit_purge.yml` | tout `audit_purge` | high |
| `grant_to_anonymous.yml` | un `grant_write` dont le sujet est `*` ou `role:anonymous` | high |
| `blocked_package_pulled.yml` | le téléchargement d'une coordonnée bloquée par un flag malware ou une verdict `MALWARE_SIGNAL` | high |
| `credential_rejected_burst.yml` | une ligne `credential_rejected` dont le `throttled_count` vaut 10 ou plus | medium |
| `sign_in_failed_burst.yml` | 5 `sign_in_failed` depuis une même `source.ip` en 10 minutes | medium |
| `denied_download_burst.yml` | 50 téléchargements refusés depuis une même `source.ip` en 5 minutes | medium |
| `bulk_pull.yml` | un même `user.id` qui télécharge 300 packages distincts en 10 minutes | medium |
| `token_new_source.yml` | tout `token_new_source` | low |
| `retention_or_cache_clear.yml` | `cache_clear`, `retention_run`, `tombstone_compact` | low |
| `config_rejected.yml` | tout `config_rejected` | low |

Les seuils sont des points de départ. `bulk_pull.yml` en particulier doit se
situer au-dessus du plus grand graphe de dépendances que vos builds résolvent,
et `retention_or_cache_clear.yml` est fait pour être restreint à l'extérieur de
votre fenêtre de maintenance, avec le filtre horaire de votre SIEM, que Sigma
ne sait pas exprimer.

Convertissez-les pour votre backend avec sigma-cli :

```bash
uvx --from sigma-cli sigma plugin install splunk
uvx --from sigma-cli sigma convert -t splunk --without-pipeline deploy/siem/sigma/
```

Les noms de champs sont déjà ECS : aucun pipeline de traitement n'est
nécessaire. Le fichier `deploy/siem/README.md` du dépôt contient un extrait
Vector et un extrait Fluent Bit pour acheminer le flux.

## Ajouter une règle

1. Écrivez-la dans `deploy/siem/sigma/`, un fichier par règle. Une règle de
   rafale tient en deux documents dans un même fichier : une règle de base
   nommée et la corrélation qui la compte.
2. Ajoutez le fichier à `deploy/siem/fixtures/expected.json` — `true` si le
   flux enregistré doit la déclencher, `false` s'il doit la laisser muette — et,
   si le flux n'a rien pour l'éprouver, ajoutez à `fixtures/stream.jsonl` des
   lignes qui la déclenchent et des lignes qui la déclenchent presque.
3. Lancez `task siem:check`. La commande exécute `sigma check` en mode strict,
   rejoue le flux à travers chaque règle, et échoue si une règle lit un champ
   que le serveur n'émet pas. Ce dernier contrôle lit la liste des champs dans
   `crates/core/src/services/audit_stream.rs` : un champ renommé casse donc le
   build au lieu d'aveugler une règle sans bruit.

## Pas encore couvert

- **La preuve d'intégrité.** Une chaîne d'empreintes signée sur le journal, et
  la règle `audit_chain_gap.yml` qui la surveille côté SIEM, arrivent avec la
  phase 5 de la RFC 0036. D'ici là, une copie du flux conservée hors de la base
  est la seule défense contre une ligne modifiée.
- **La rétention et la pseudonymisation** des enregistrements eux-mêmes sont
  la phase 4. Le flux n'est pas concerné : ce qu'un collecteur a stocké relève
  de sa propre rétention.
