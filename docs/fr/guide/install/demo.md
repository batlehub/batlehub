---
sourcePath: guide/install/demo.md
sourceHash: 89026564feb11a4f
---

# Déployer la démo

Une commande monte un BatleHub complet sur une seule machine, depuis une copie
du dépôt : le serveur, sa base, un analyseur, cinq registres, six profils — et
du trafic réel, pour que chaque page de la console ait quelque chose à
montrer, y compris les états qu'une évaluation atteint rarement d'elle-même :
une version refusée pour malware, un signalement d'un SOC sur une version déjà
téléchargée, un registre que personne n'a protégé, un jeton statique conservé
en clair.

C'est une démo, pas une installation. Pour un serveur que vous garderez, voir
[Installation](/fr/guide/installation).

## Prérequis

- une copie du dépôt, et [mise](https://mise.jdx.dev) avec les outils du dépôt
  installés (`mise install`) : Rust, Node et pnpm en viennent, et PostgreSQL
  aussi quand aucun ne tourne ;
- `python3` et `curl` ;
- le réseau : la démo sert de proxy à npmjs.org, pypi.org, crates.io et
  proxy.golang.org ;
- quelques minutes pour la première compilation, et la place d'un build de
  debug sous `target/`.

PostgreSQL est facultatif. Le script utilise, dans cet ordre, `$DATABASE_URL`,
un serveur sur `127.0.0.1:5432` (`batlehub` / `changeme`, celui du conteneur de
développement), ou démarre son propre PostgreSQL 17 sur `127.0.0.1:5433` dans
`~/.local/share/pg-fallback`, sans root.

## La démarrer

```sh
scripts/demo.sh start
```

Le script compile le serveur, la CLI et la console, écrit sa configuration
dans `tmp/demo/config.toml`, démarre l'analyseur et le serveur, injecte les
données, redémarre le serveur une fois, puis affiche l'adresse et les jetons.
Tout ce qu'il écrit vit sous `tmp/demo/`, que git ignore.

La console est servie par le serveur lui-même, sur la même origine que l'API :
`http://127.0.0.1:8080`. Dans un environnement de développement en ligne, le
port 8080 est celui que l'espace de travail expose, et le script affiche aussi
son adresse publique.

| Commande | Ce qu'elle fait |
| --- | --- |
| `scripts/demo.sh start` | compiler, démarrer, injecter |
| `scripts/demo.sh start --reset` | la même chose sur une base et un cache vides |
| `scripts/demo.sh start --no-build` | réutiliser les binaires et la console déjà compilés |
| `scripts/demo.sh start --no-seed` | démarrer sans trafic |
| `scripts/demo.sh seed` | ajouter une tournée de trafic à une démo en cours |
| `scripts/demo.sh status` | lequel du serveur et de l'analyseur tourne |
| `scripts/demo.sh logs` | suivre le journal du serveur |
| `scripts/demo.sh stop` | arrêter le serveur et l'analyseur ; la base reste |

| Variable | Par défaut | Sens |
| --- | --- | --- |
| `DATABASE_URL` | — | un PostgreSQL à utiliser ; le script crée `batlehub_demo` à côté de la base qu'elle nomme |
| `DEMO_PORT` | `8080` | le port du serveur |
| `DEMO_OSV_PORT` | `8197` | le port de l'analyseur |
| `CARGO_BUILD_JOBS` | `2` | tâches de compilation en parallèle |

## Ce qu'elle monte

**Un analyseur qui répond toujours pareil.** L'OSV de la démo rejoue les
réponses qu'api.osv.dev a données pour un ensemble de vrais paquets,
enregistrées dans `tests/fixtures/supply-chain-jdd/osv/`. Les constats sont de
vrais avis — `event-stream@3.3.6`, `ua-parser-js@0.7.29`, `minimist@1.2.5`,
`lodash@4.17.20` — et la démo retient et refuse les mêmes versions à chaque
lancement, quoi que dise la base en ligne ce jour-là. Un paquet que les
enregistrements ne nomment pas est sain.

**Cinq registres, un par état de sécurité :**

| Registre | Type | Protection | Ce qu'il montre |
| --- | --- | --- | --- |
| `npm-public` | npm | refuse *high* et au-delà ; reçoit les signalements du SOC | des retenues pour malware et vulnérabilités, un signalement qui bloque les téléchargements |
| `crates-io` | cargo | refuse *high* et au-delà, avec un `cve_gate` | la colonne *Contrôles* de la couverture |
| `pypi-public` | PyPI | avertit : un constat *critical* est servi avec un avertissement, jamais refusé ; un malware connu l'est toujours | un registre qui sert avec un avertissement |
| `npm-young` | npm | retient chaque version sur son âge | des retenues en attente de `MIN_AGE_NOT_MET` |
| `go-public` | Go | aucune | la ligne *non protégé* de la couverture, et son `ATTENTION` |

**Six profils**, chacun un jeton avec lequel se connecter :

| Jeton | Qui | Ce qu'il voit |
| --- | --- | --- |
| `demo-admin-token` | un administrateur | tout, le tableau de bord et la sécurité de l'instance |
| `demo-secops-token` | une équipe sécurité déléguée (RFC 0042) | tout `/security` ; des pages d'administration, seulement le journal d'audit |
| `demo-soc-token` | un SOC qui ne lit que les signalements | `/security` : la vue d'ensemble et *Signalements* |
| `demo-auditor-token` | un auditeur | `/security` s'ouvre sur *Exposition* ; le journal d'audit |
| `demo-user-token` | un développeur | le catalogue ; ni `/security` ni `/admin` |
| `demo-ci-token` | une tâche de CI | un utilisateur simple, dont le jeton est haché dans la config |

Le jeton de l'administrateur est conservé en clair exprès : il lève
l'avertissement `auth.plain-static-token` que signale la sécurité de
l'instance. Le jeton de CI montre l'autre cas — une empreinte Argon2id, aucun
avertissement.

## Ce que fait l'injection

Chaque étape est une vraie requête à travers le serveur ; rien n'est écrit en
base à la main.

1. **Premiers téléchargements** sur les cinq registres. Chaque version d'un
   registre protégé est retenue `SCAN_PENDING` et mise en file.
2. **Le worker analyse** — le script attend que la file soit vide.
3. **Seconds téléchargements.** Les versions qui ont un constat sont refusées
   désormais : ce sont les refus que comptent les tendances et l'exposition.
4. **Un SOC pousse deux signalements signés** : un `hard_block` sur
   `left-pad@1.3.0`, déjà téléchargé et refusé quand il est redemandé, et un
   `warn` sur `minimist@1.2.8`, téléchargé avant le signalement puis trois fois
   après — un avertissement sert toujours, et seul un téléchargement servi
   compte comme tel.
5. **24 identifiants refusés**, venus de jetons que personne n'a émis.
6. **Un bannissement manuel** de `203.0.113.7`, une adresse de documentation.
7. **Une exemption** sur `minimist@1.2.5` qui prend fin dans trois jours.

Le serveur est ensuite redémarré une fois : le cumul quotidien de sécurité
tourne au démarrage, si bien que `/security/trends` a un premier jour.

## Une visite

Connecté avec `demo-secops-token` :

- `/security` — la bande d'état : *Retenues*, *Signalements* et *Couverture* à
  `ATTENTION`, *Analyseurs et worker* `OK` ; dessous, *À décider* nomme
  `minimist@1.2.8` téléchargé après son signalement, l'exemption qui expire et
  les retenues derrière un blocage.
- `/security/holds`, `/security/findings` — les codes de motif en libellés,
  le code brut à côté de chacun.
- `/security/flags`, `/security/exposure` — qui a tiré `left-pad@1.3.0` avant
  son blocage, et `minimist@1.2.8` avant et après son avertissement.
- `/security/coverage` — quatre registres protégés, `go-public` non ;
  `/security/coverage/npm-public` détaille son profil.
- `/security/scanners` — le worker, la file, le dernier succès de l'analyseur
  `osv`.
- `/security/trends` — le premier jour ; passez en *Afficher en tableau*.

Connecté avec `demo-admin-token` :

- `/admin/dashboard` — la phrase du cache, puis une ligne par domaine de
  sécurité, et *Config* avec les avertissements de sécurité.
- `/admin/instance-security` — les identifiants refusés, le bannissement, le
  jeton en clair et comment le hacher.

Connecté avec `demo-soc-token`, ouvrez `/security/holds` : vous arrivez sur
`/security/flags`, la seule page de ce groupe que ce jeton lit.

Depuis un terminal :

```sh
BATLEHUB_SERVER=http://127.0.0.1:8080 BATLEHUB_TOKEN=demo-secops-token \
  target/debug/batlehub-cli security status; echo "exit $?"
```

`1` signifie qu'une ligne est à `ATTENTION`, `7` qu'une ligne est `UNKNOWN` ou
illisible pour le jeton, et `ATTENTION` l'emporte. Avec `demo-soc-token`, seule
la ligne *Signalements* s'affiche, une ligne `not shown:` nomme les trois
autres, et le code de sortie reste `1` : le blocage met *Signalements* à
`ATTENTION`.

## Ce que la démo ne montre pas

- **Un bannissement automatique.** Le seuil est réglé bien au-dessus de ce que
  l'injection envoie : derrière l'ingress d'un environnement de développement,
  tous les navigateurs partagent une adresse, et un bannissement automatique
  fermerait la démo à tout le monde.
- **Un jeton utilisé depuis une nouvelle adresse.** Ce signal concerne les
  jetons utilisateur qu'une personne crée après s'être connectée par
  authentification unique, que la démo ne configure pas.
- **L'historique d'avant le premier lancement.** Les versions retenues sont
  comptées à partir du jour où la démo démarre ; refus, signalements et
  analyses aussi loin que remontent leurs propres traces, c'est-à-dire la
  première minute de l'injection.

## Quand ça se passe mal

| Symptôme | Où regarder |
| --- | --- |
| `cargo build failed` | `tmp/demo/build-rust.log` |
| `the console build failed` | `tmp/demo/build-ui.log` |
| `port 8080 is taken` | un autre serveur y tourne : `DEMO_PORT=8090 scripts/demo.sh start` |
| `the server exited` | `tmp/demo/server.log` — le plus souvent la base ou une clé de configuration |
| la file ne s'est pas vidée | les lignes du worker dans `tmp/demo/server.log` ; la vue d'ensemble montre ce qui reste |
| les téléchargements répondent `0` ou `502` | le réseau : la démo a besoin des quatre registres publics |

## La retirer

```sh
scripts/demo.sh stop
rm -rf tmp/demo
```

La base `batlehub_demo` reste ; `--reset` la supprime au démarrage suivant. Un
PostgreSQL que le script a démarré lui-même s'arrête avec
`mise x conda:postgresql@17 -- pg_ctl -D ~/.local/share/pg-fallback/data stop`.
