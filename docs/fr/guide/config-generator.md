---
aside: false
pageClass: page-config-gen
sourcePath: guide/config-generator.md
sourceHash: 6726f9b56007b4ae
---

# Générateur de configuration

Remplissez le formulaire ci-dessous pour générer un `config.toml` pour votre
instance BatleHub. L'aperçu se met à jour à la frappe — à côté du formulaire
quand la fenêtre est assez large, en dessous sinon.

Une fois terminé, cliquez sur **Download** pour enregistrer le fichier, ou sur
**Copy** pour le coller dans votre éditeur.

::: info Et ensuite ?
Placez le fichier généré au chemin que vous passez à `--config` (par défaut
`./config.toml`), puis démarrez BatleHub. Pour un déploiement Helm, collez son
contenu dans votre `values.yaml` sous `registriesRaw` et renseignez les champs
individuels. Voir [Administration](/fr/guide/administration) pour la référence
complète de chaque option.
:::

<ClientOnly>
  <ConfigGenerator />
</ClientOnly>
