/**
 * La navigation française — même forme que `nav/en.ts`, traduite.
 *
 * Trois règles tiennent ce fichier, et les portes de garde les vérifient :
 *
 *   - un lien vers une page traduite est préfixé `/fr/` ; un lien vers une page
 *     qui reste en anglais est écrit en absolu sans préfixe. VitePress ne fait
 *     aucun repli : une entrée `/fr/…` qui ne correspond à aucun fichier sous
 *     `docs/fr/` est un 404, pas un retour silencieux vers l'anglais.
 *
 *   - `contributing/`, `rfc/` et la feuille de route ne se traduisent pas —
 *     décision de périmètre, pas oubli. Les deux premiers s'adressent à qui
 *     modifie le code ; la troisième est générée depuis `ROADMAP.md`, qui est la
 *     source canonique et est en anglais. Les trois sont donc listées ici avec
 *     leur URL anglaise, et `check-links.mjs` connaît cette exception.
 *
 *   - un libellé se nomme d'après ce que la page fait, jamais d'après la
 *     traduction mot à mot de son titre anglais. Le vocabulaire vient de
 *     `ui/src/locales/fr.json` : registre, amont, artefact, paquet, garde-fou,
 *     stockage. Le glossaire arbitre — `docs/internal/glossaire-fr.md`.
 */

// La barre de navigation liste les points d'entrée de chaque espace, en clair.
// Le détail vit dans la barre latérale, de sorte que les deux ne répètent pas la
// même liste. Pas d'entrée « Accueil » : le logo à gauche y mène déjà.
export const nav = [
  {
    text: "Installer",
    link: "/fr/guide/installation",
    activeMatch: "/fr/guide/installation",
  },
  // La seule page qui est un outil plutôt qu'un document. Elle est dans la barre
  // de navigation parce qu'un visiteur qui a décidé d'installer a besoin d'un
  // `config.toml` juste après, et que le générer vaut mieux que lire une
  // référence de 12 000 mots pour l'écrire à la main.
  {
    text: "Générateur de config",
    link: "/fr/guide/config-generator",
    activeMatch: "/fr/guide/config-generator",
  },
  {
    text: "Guide utilisateur",
    link: "/fr/use/",
    activeMatch: "/fr/use/",
  },
  {
    text: "Registres",
    link: "/fr/registries/",
    activeMatch: "/fr/registries/",
  },
  {
    text: "Administration",
    link: "/fr/guide/administration",
    activeMatch: "/fr/guide/admin",
  },
  {
    text: "Exploitation",
    link: "/fr/operations/",
    activeMatch: "/fr/operations/",
  },
  // Trois espaces qui parlent du projet plutôt que de son exploitation, et qui
  // restent en anglais. Un menu déroulant plutôt que trois entrées de plus :
  // personne n'arrive sur la documentation d'un proxy de paquets en cherchant
  // l'index des RFC.
  {
    text: "Projet",
    items: [
      { text: "Feuille de route (en)", link: "/guide/roadmap" },
      { text: "Contribuer (en)", link: "/contributing/" },
      { text: "Historique de conception (en)", link: "/rfc/" },
    ],
    activeMatch: "/(contributing|rfc)/",
  },
];

export const sidebar = {
  "/fr/registries/": [
    {
      text: "Registres",
      items: [{ text: "Vue d'ensemble et matrice", link: "/fr/registries/" }],
    },
    {
      text: "Hébergement de code source",
      items: [
        { text: "GitHub", link: "/fr/registries/github" },
        { text: "Forgejo / Gitea", link: "/fr/registries/forgejo" },
        { text: "GitLab", link: "/fr/registries/gitlab" },
      ],
    },
    {
      text: "Gestionnaires de paquets par langage",
      items: [
        { text: "npm", link: "/fr/registries/npm" },
        { text: "Cargo", link: "/fr/registries/cargo" },
        { text: "Modules Go", link: "/fr/registries/goproxy" },
        { text: "Maven", link: "/fr/registries/maven" },
        { text: "PyPI", link: "/fr/registries/pypi" },
        { text: "Conda", link: "/fr/registries/conda" },
        { text: "Composer", link: "/fr/registries/composer" },
        { text: "RubyGems", link: "/fr/registries/rubygems" },
        { text: "NuGet", link: "/fr/registries/nuget" },
        { text: "Terraform", link: "/fr/registries/terraform" },
      ],
    },
    {
      text: "Extensions d'éditeur",
      items: [
        { text: "OpenVSX", link: "/fr/registries/openvsx" },
        {
          text: "Place de marché VS Code",
          link: "/fr/registries/vscode-marketplace",
        },
        {
          text: "Place de marché JetBrains",
          link: "/fr/registries/jetbrains-marketplace",
        },
      ],
    },
    {
      text: "Paquets système",
      items: [
        { text: "Debian / APT", link: "/fr/registries/deb" },
        { text: "RPM / YUM / DNF", link: "/fr/registries/rpm" },
        { text: "Pacman / Arch", link: "/fr/registries/pacman" },
      ],
    },
    {
      text: "Binaires et miroirs",
      items: [
        { text: "IDE JetBrains", link: "/fr/registries/jetbrains" },
        { text: "Miroir générique", link: "/fr/registries/generic" },
      ],
    },
    {
      text: "Chaînes d'outils",
      items: [
        { text: "Node (nvm, fnm, n, mise)", link: "/fr/registries/nodedist" },
        { text: "SDKMAN", link: "/fr/registries/sdkman" },
      ],
    },
  ],

  // J'exploite ce serveur.
  "/fr/guide/": [
    {
      text: "Pour commencer",
      items: [
        { text: "Ce que fait BatleHub", link: "/fr/guide/features" },
        { text: "Installation", link: "/fr/guide/installation" },
        { text: "Configuration", link: "/fr/guide/configuration" },
        {
          text: "Exemples commentés",
          link: "/fr/guide/configuration-examples",
        },
        { text: "Générateur de config", link: "/fr/guide/config-generator" },
      ],
    },
    {
      text: "Administration",
      items: [
        { text: "Vue d'ensemble", link: "/fr/guide/administration" },
        { text: "Configuration", link: "/fr/guide/admin-config" },
        { text: "Stockage et santé", link: "/fr/guide/admin-storage-health" },
        { text: "Politiques et paquets", link: "/fr/guide/admin-policies" },
        { text: "Accès et audit", link: "/fr/guide/admin-access" },
      ],
    },
    {
      text: "Référence",
      items: [
        { text: "Mise en cache", link: "/fr/guide/caching" },
        { text: "Contrôle d'accès", link: "/fr/guide/access-control" },
        { text: "Routage par hôte", link: "/fr/guide/host-routing" },
        { text: "Amonts privés", link: "/fr/guide/private-upstreams" },
        { text: "Rechargement à chaud", link: "/fr/guide/hot-reload" },
        { text: "SBOM", link: "/fr/guide/sbom" },
        { text: "Haute disponibilité", link: "/fr/guide/high-availability" },
        {
          text: "Sous-commandes du binaire serveur",
          link: "/fr/guide/server-cli",
        },
        { text: "Dimensionnement", link: "/fr/guide/capacity-planning" },
      ],
    },
    {
      // Générée depuis `ROADMAP.md`, qui est canonique et en anglais : la page
      // française serait une copie qu'aucune porte ne tient à jour.
      text: "Projet",
      items: [{ text: "Feuille de route (en)", link: "/guide/roadmap" }],
    },
  ],

  // J'ai un gestionnaire de paquets et un token, et il faut que ça marche.
  "/fr/use/": [
    {
      text: "Utiliser BatleHub",
      items: [
        { text: "Vue d'ensemble", link: "/fr/use/" },
        { text: "Publier des paquets", link: "/fr/use/publishing" },
        { text: "Client en ligne de commande", link: "/fr/use/cli" },
      ],
    },
    {
      text: "Explorateur de paquets",
      items: [
        { text: "Vue d'ensemble", link: "/fr/use/package-explorer" },
        {
          text: "Recherche en amont",
          link: "/fr/use/package-explorer-search",
        },
        {
          text: "Contrôle d'accès",
          link: "/fr/use/package-explorer-access",
        },
        { text: "Cache et API", link: "/fr/use/package-explorer-cache" },
      ],
    },
    {
      text: "Quand ça ne marche pas",
      items: [
        {
          text: "Proxy de vulnérabilités",
          link: "/fr/use/vulnerability-proxy",
        },
        { text: "mise", link: "/fr/use/mise" },
        { text: "Dépannage", link: "/fr/use/troubleshooting" },
      ],
    },
  ],

  // Quelque chose est cassé, ou un auditeur pose des questions.
  "/fr/operations/": [
    {
      text: "Exploitation",
      items: [{ text: "Vue d'ensemble", link: "/fr/operations/" }],
    },
    {
      text: "Procédures",
      items: [
        {
          text: "Réponse à incident",
          link: "/fr/operations/incident-response",
        },
        {
          text: "Reprise après sinistre",
          link: "/fr/operations/disaster-recovery",
        },
        { text: "Ce qui sort de cette instance", link: "/fr/operations/egress" },
        {
          text: "Durcissement en production",
          link: "/fr/operations/production-hardening",
        },
        {
          text: "Contrôle de santé des registres",
          link: "/fr/operations/check-registries",
        },
        { text: "Coupure réseau", link: "/fr/operations/air-gap" },
        {
          text: "Disparition d'un amont",
          link: "/fr/operations/upstream-disappearance",
        },
      ],
    },
    {
      text: "Conformité",
      items: [
        {
          text: "Gestion du changement",
          link: "/fr/operations/change-management",
        },
        { text: "Checklist SOC 2", link: "/fr/operations/soc2-checklist" },
        { text: "MD5 et SHA-1", link: "/fr/operations/weak-hashes" },
      ],
    },
  ],
};
