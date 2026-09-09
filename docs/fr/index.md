---
layout: home
sourcePath: index.md
sourceHash: 7211cfa21b239cfc

hero:
  image:
    src: /logo.svg
    alt: BatleHub
  name: BatleHub
  text: Votre hub de paquets. Proxy, cache et hébergement.
  tagline: Placez-le entre vos outils de build et Internet. Mettez les artefacts en cache, appliquez le contrôle d'accès et publiez vos paquets privés — depuis un seul serveur auto-hébergé.
  actions:
    - theme: brand
      text: Commencer
      link: /fr/guide/installation
    - theme: alt
      text: Voir sur Git
      link: https://github.com/batlehub/batlehub

features:
  - icon: ⚡
    title: Mettez en cache ce que vous téléchargez déjà
    details: Chaque artefact est récupéré une fois en amont, puis servi depuis le disque ou S3. Vingt et un écosystèmes, un seul serveur, et rien à changer à la façon dont vos outils de build sont lancés.
    link: /fr/guide/caching
    linkText: Comment fonctionne le cache
  - icon: 🔒
    title: Publiez ce qui est à vous
    details: Paquets npm privés, crates Cargo, modules Go, wheels Python, paquets NuGet et bien d'autres — sur le même serveur, dans le même espace d'URL, publiés avec l'outil que vous utilisez déjà.
    link: /fr/use/publishing
    linkText: Guide de publication
  - icon: 🛡️
    title: Décidez qui obtient quoi
    details: Permissions par registre pour les rôles anonyme, utilisateur et administrateur, groupes issus d'OIDC ou de tokens de CI, et garde-fous sur ce qui peut être récupéré — par âge, par alerte de sécurité, par licence.
    link: /fr/guide/access-control
    linkText: Contrôle d'accès
---

BatleHub sert par proxy, met en cache et héberge en privé **21 types de
registres** — de npm et Cargo à Debian, Terraform et la place de marché VS Code.
La **[référence des registres](/fr/registries/)** donne la matrice des
fonctionnalités et une page de mise en place pour chacun ; la **[liste complète
des fonctionnalités](/fr/guide/features)** dit tout ce que le serveur sait faire.
