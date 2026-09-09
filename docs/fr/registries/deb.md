---
sourcePath: registries/deb.md
sourceHash: 3cfaf209d52ee15b
---

# Debian / APT

Fait proxy d'un dépôt APT Debian ou Ubuntu et, en mode `local` ou `hybrid`,
héberge le vôtre : publiez des paquets `.deb` et BatleHub régénère les index
`Packages` et `Release`, en les signant avec une clé OpenPGP Ed25519 quand
`[registries.repo_signing]` est configuré.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `deb` |
| **Amont par défaut** | aucun — déclarez `upstreams` explicitement pour proxy et hybrid |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par chemin |
| **Publication privée** | ✅ `curl -X PUT … /deb/pool/{suite}/{component}/upload` |
| **Coupure réseau** | pas d'index composé hors ligne : un fichier `Packages` signé ne peut pas être re-signé ici ; un fichier détenu est servi par chemin |

## Mise en place du proxy

Ajoutez une ligne de source sous `/etc/apt/sources.list.d/`. Remplacez
`<registry>` par le nom de registre que vous avez configuré ; la suite
(`stable`) et le composant (`main`) doivent correspondre à la disposition de
l'amont (ou à celle que vous publiez localement) :

```bash
REG="https://batlehub.example.com/proxy/<registry>/deb"

# Importer la clé de signature (dépôts local/hybrid signés uniquement)
curl -fsSL $REG/key.gpg | sudo tee /usr/share/keyrings/<registry>.asc >/dev/null

# Ajouter la source
echo "deb [signed-by=/usr/share/keyrings/<registry>.asc] $REG stable main" \
  | sudo tee /etc/apt/sources.list.d/<registry>.list

sudo apt update && sudo apt install hello
```

Pour un dépôt **local** non signé (pas de clé `[registries.repo_signing]`),
remplacez `[signed-by=…]` par `[trusted=yes]`.

::: warning `trusted=yes` désactive apt-secure
`trusted=yes` dit à apt d'accepter le dépôt **sans aucune vérification de
signature** — tout ce qui peut répondre à la place de l'hôte, ou se placer sur le
trajet, peut servir des paquets arbitraires qui s'installent en root. Réservez-le
à un canal isolé et entièrement de confiance (un réseau interne que vous
maîtrisez de bout en bout). Préférez configurer `[registries.repo_signing]` pour
que BatleHub signe les index et que les consommateurs vérifient avec
`signed-by`.
:::

Le **mode proxy** n'a pas de clé BatleHub — `…/deb/key.gpg` n'est servi que pour
les registres `local` et `hybrid` dotés d'une clé `repo_signing`. En mode proxy,
BatleHub relaie les `InRelease` et `Release.gpg` du dépôt **amont** et leur
signature : apt vérifie donc contre la clé d'archive **de l'amont**. Les miroirs
officiels Debian et Ubuntu la fournissent déjà (paquets
`debian-archive-keyring` et `ubuntu-keyring`) :

```bash
echo "deb [signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] \
  https://batlehub.example.com/proxy/<registry>/deb stable main" \
  | sudo tee /etc/apt/sources.list.d/<registry>.list
```

## Publication (local / hybrid) {#publishing-local-hybrid}

Envoyez un `.deb` par un `PUT`. La distribution et le composant viennent du
chemin d'envoi ; BatleHub en déduit l'emplacement dans le pool, régénère les
index de la suite et re-signe `InRelease` et `Release.gpg` :

```bash
curl -X PUT \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  --data-binary @hello_1.0_amd64.deb \
  https://batlehub.example.com/proxy/<registry>/deb/pool/stable/main/upload
```

La signature exige une clé OpenPGP Ed25519 configurée sous
`[registries.repo_signing]` ; sans elle, les index générés sont servis non signés
(à consommer avec `[trusted=yes]`).

## Authentification

APT lit ses identifiants dans `/etc/apt/auth.conf.d/` (Debian 9+ / Ubuntu
19.04+). L'entrée de `sources.list` reste inchangée — les identifiants vivent
dans un fichier à part, invisible pour `apt-cache policy` :

```bash
sudo tee /etc/apt/auth.conf.d/batlehub.conf > /dev/null <<'EOF'
machine batlehub.example.com
login <your-username>
password <your-token>
EOF
sudo chmod 0600 /etc/apt/auth.conf.d/batlehub.conf
```

Sur des systèmes plus anciens, utilisez `/etc/apt/auth.conf` avec la même
strophe `machine / login / password`. Vous pouvez aussi embarquer les
identifiants directement dans l'URL (moins sûr — le token apparaît dans la sortie
d'`apt-cache policy`) :
`https://<user>:<token>@batlehub.example.com/proxy/<registry>/deb …`.

## Notes

- Une erreur `NO_PUBKEY` ou « the following signatures couldn't be verified » en
  mode proxy signifie que la clé de l'amont n'est pas dans le trousseau nommé par
  `signed-by` — installez `debian-archive-keyring` (Debian) ou `ubuntu-keyring`
  (Ubuntu), ou importez la clé de l'amont dans un trousseau et faites pointer
  `signed-by` dessus. Authentifiez-vous contre le trousseau de l'amont plutôt que
  de recourir à `[trusted=yes]` : en mode proxy, BatleHub relaie la signature
  amont, la vérification est donc disponible et la désactiver n'apporte rien.
- Publier exige que le registre soit en mode `local` ou `hybrid` — demandez à
  votre administrateur.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
