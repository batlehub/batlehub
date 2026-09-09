---
sourcePath: operations/index.md
sourceHash: c9f30a203e5250b1
---

# Exploitation

Pour la personne d'astreinte, et pour l'auditeur.

Tout ce qui suit suppose que BatleHub tourne déjà. Si vous en êtes encore à la
mise en place, commencez par [Installation](/fr/guide/installation) et
[Configuration](/fr/guide/configuration).

::: warning Des repères, pas un engagement
BatleHub est un logiciel auto-hébergé sous licence Apache 2.0, et **c'est vous
l'opérateur**. Toutes les pages de cet espace sont du matériel pour vous aider à
exploiter votre propre instance : ce que le logiciel rend possible, ce que le
projet fait lui-même, et à quoi ressemble une procédure raisonnable.

Rien de tout cela n'est un accord de niveau de service, un engagement de support,
une garantie ou une certification. Il n'y a personne d'astreinte à part vous. Les
seuils de gravité, les destinataires des notifications, les durées de rétention
et les objectifs de reprise sont des exemples à adapter, pas des obligations que
le projet prend à sa charge — adaptez-les à vos propres politiques et à ce que
votre organisation a réellement convenu avec ses utilisateurs.
:::

## Procédures

- **[Réponse à incident](/fr/operations/incident-response)** — quoi faire quand
  BatleHub est en panne, lent, ou sert la mauvaise chose, et qui prévenir.
- **[Reprise après sinistre](/fr/operations/disaster-recovery)** — restaurer
  depuis des sauvegardes, et ce que « restauré » veut dire pour la base, le
  magasin d'artefacts et le cache.
- **[Durcissement en production](/fr/operations/production-hardening)** — les
  réglages qui distinguent une instance qui marche d'une instance que vous
  mettriez devant une entreprise.
- **[Ce qui sort de cette instance](/fr/operations/egress)** — toutes les
  requêtes sortantes que ce serveur émet, ce qui les déclenche, et comment les
  arrêter.
- **[Le worker d'analyse](/fr/operations/scan-worker)** — le rôle qui analyse ce
  que le proxy s'apprête à servir : la file, le bac à sable, et ce que veut dire
  une installation retenue.

## Conformité

- **[Gestion du changement](/fr/operations/change-management)** — comment un
  changement atteint la production, et ce qui en est enregistré.
- **[Checklist SOC 2](/fr/operations/soc2-checklist)** — les contrôles qu'un
  auditeur demande, mis en regard de ce que BatleHub fait réellement.
- **[MD5 et SHA-1](/fr/operations/weak-hashes)** — chaque empreinte faible qu'un
  scanner trouve, à quoi elle sert, et si le protocole permet mieux.

À lire aussi dans le guide : [Haute disponibilité](/fr/guide/high-availability),
[SBOM](/fr/guide/sbom) et
[Vulnerability scanning](/contributing/security-scanning) (en anglais).
