# French catalogue — review sheet

Generated from `ui/src/locales/{en,fr}.json`. Regenerate with `task ui:i18n:review`.

## How to review this

You do not need to check every row. The ones that matter, in order:

1. **Rows with a note.** Judgement calls I made, or strings that grew noticeably
   longer in French and may overflow a layout sized for English.
2. **`destructive.*`.** These are the words between an operator and a permanent
   deletion. If any reads as softer in French than in English, that is a safety
   bug, not a style preference.
3. **Anything that names a thing you type.** `config.toml`, `[[registries]]`, `ConfigMap`, `Helm`, `TOML`, `CLI` must appear
   verbatim in both columns — a test enforces this, but the test only knows the
   terms it was told about.

Mark a row by replacing the French with yours; nothing else needs changing.

## The rule applied

Translate the sentence, never the domain term. A French UI that renames `yank`,
`latest`, a registry mode, or a config key leaves the reader unable to search for
it, type it, or match it against the docs — which is worse than English.

## Strings (647)

| Key | English | French | Note |
| --- | --- | --- | --- |
| `a11y.breadcrumb` | Breadcrumb | Fil d'Ariane | **Decided: `Fil d'Ariane`**, the established French term. Screen-reader only. |
| `a11y.closeMenu` | Close menu | Fermer le menu |  |
| `a11y.openMenu` | Open menu | Ouvrir le menu |  |
| `a11y.pagination` | Pagination | Pagination |  |
| `a11y.primaryNav` | Primary | Principale |  |
| `a11y.sectionsOf` | {title} sections | Sections : {title} |  |
| `a11y.skipToContent` | Skip to content | Aller au contenu | Screen-reader only. Never seen visually, but read aloud. |
| `a11y.tableScroll` | Table, scrollable | Tableau, défilable |  |
| `accessCheck.accessCheck` | Access Check | Vérification d'accès |  |
| `accessCheck.allowed` | Allowed | Autorisé |  |
| `accessCheck.artifactOptional` | Artifact (optional) | Artefact (facultatif) |  |
| `accessCheck.checkAccess` | Check Access | Vérifier l'accès |  |
| `accessCheck.checking` | Checking… | Vérification… |  |
| `accessCheck.denied` | Denied | Refusé |  |
| `accessCheck.nameOwnerRepo` | Name (owner/repo) | Nom (owner/repo) |  |
| `accessCheck.noReasonGiven` | no reason given | aucun motif indiqué |  |
| `account.cli` | CLI | CLI | **Kept verbatim.** It is the binary's name. |
| `account.description` | Your profile, tokens, namespace and CLI. | Votre profil, vos tokens, votre namespace et le CLI. | **30% longer than English** — check it does not overflow. |
| `account.namespace` | Namespace | Namespace | **Kept verbatim.** Matches the config key and the admin surface (`Team Namespaces`). |
| `account.profile` | Profile | Profil |  |
| `account.title` | My account | Mon compte |  |
| `account.tokens` | Tokens | Tokens | **Kept verbatim.** `Token` is what the API, the CLI flag (`--token`) and the docs call it. `Jeton` would be correct French and unsearchable. |
| `adminAccessCheck.allow` | ✓ ALLOW | ✓ AUTORISÉ | **Translated to `✓ AUTORISÉ`.** These are policy verdicts shown to a human, not config values — unlike the registry modes, nobody types them. |
| `adminAccessCheck.checkAccess` | Check access | Vérifier l'accès |  |
| `adminAccessCheck.commaSeparated` | (comma-separated) | (séparés par des virgules) |  |
| `adminAccessCheck.deny` | ✗ DENY | ✗ REFUSÉ | Same as `adminAccessCheck.allow`. |
| `adminAccessCheck.oidc1TeamATeam` | oidc1:team-a, team-b | oidc1:team-a, team-b |  |
| `adminAccessCheck.optional` | (optional) | (facultatif) |  |
| `adminAccessCheck.packageName` | Package name | Nom du paquet |  |
| `adminAccessCheck.rbacAccessCheck` | RBAC Access Check | Vérification d'accès RBAC |  |
| `adminAccessCheck.resourceType` | Resource type | Type de ressource |  |
| `adminAccessCheck.simulateWhetherAnIdentityWould` | Simulate whether an identity would be allowed to access a package resource under the current registry policy — without making a real request. | Simuler si une identité serait autorisée à accéder à une ressource de paquet selon la politique de registre actuelle — sans envoyer de vraie requête. |  |
| `adminAccessCheck.simulatedIdentity` | Simulated identity | Identité simulée |  |
| `adminAccessCheck.userId` | User ID | Identifiant utilisateur |  |
| `adminBetaChannel.addBetaChannelMember` | Add beta channel member | Ajouter un membre au canal bêta | **35% longer than English** — check it does not overflow. |
| `adminBetaChannel.addMember` | Add member | Ajouter le membre |  |
| `adminBetaChannel.addToBetaChannelFor` | Add a user or group to the beta channel for {registry}. | Ajouter un utilisateur ou un groupe au canal bêta de {registry}. |  |
| `adminBetaChannel.adding` | Adding… | Ajout… |  |
| `adminBetaChannel.betaChannel` | Beta Channel | Canal bêta |  |
| `adminBetaChannel.grantedBy` | Granted by | Accordé par |  |
| `adminBetaChannel.manageWhoCanAccessPre` | Manage who can access pre-release versions in each registry. | Gérer qui peut accéder aux versions pre-release dans chaque registre. |  |
| `adminBetaChannel.noBetaChannelMembersFor` | No beta channel members for this registry. | Aucun membre du canal bêta pour ce registre. |  |
| `adminBetaChannel.optionalYourUserId` | Optional — your user ID | Facultatif — votre ID utilisateur | **43% longer than English** — check it does not overflow. |
| `adminBetaChannel.principalId` | Principal ID | Identifiant du principal |  |
| `adminBetaChannel.removeMember` | Remove member? | Retirer ce membre ? |  |
| `adminBetaChannel.removing` | Removing… | Retrait… |  |
| `adminBetaChannel.selectARegistry` | Select a registry… | Choisir un registre… |  |
| `adminBetaChannel.selectARegistryToView` | Select a registry to view members. | Choisissez un registre pour voir les membres. | **32% longer than English** — check it does not overflow. |
| `adminBetaChannel.willLoseAccess` | {principal} will lose access to pre-release versions in {registry}. | {principal} perdra l'accès aux versions pre-release dans {registry}. |  |
| `adminBulk.blockOrUnblockMultiplePackages` | Block or unblock multiple packages at once by pasting or uploading a CSV file. | Bloquer ou débloquer plusieurs paquets à la fois en collant ou en téléversant un fichier CSV. |  |
| `adminBulk.bulkBlock` | Bulk Block | Blocage de masse |  |
| `adminBulk.configureAction` | Configure action | Configurer l'action |  |
| `adminBulk.csvFormat` | CSV format | Format CSV |  |
| `adminBulk.csvNotes` | Header row is optional. {artifact} may be left blank for version-level blocks. {reason} is used only for block actions. | La ligne d'en-tête est facultative. {artifact} peut rester vide pour les blocages au niveau version. {reason} n'est utilisé que pour les actions de blocage. | **31% longer than English** — check it does not overflow. |
| `adminBulk.defaultReason` | Default reason | Motif par défaut |  |
| `adminBulk.doneSummary` | Done — {succeeded} succeeded, {failed} failed | Terminé — {succeeded} réussis, {failed} échoués |  |
| `adminBulk.orPasteAbove` | or paste above | ou collez ci-dessus |  |
| `adminBulk.pasteCsv` | Paste CSV | Coller le CSV |  |
| `adminBulk.previewRows` | Preview rows | Aperçu des lignes |  |
| `adminBulk.processing` | Processing… | Traitement… |  |
| `adminBulk.uploadCsvFile` | Upload .csv file | Envoyer un fichier .csv |  |
| `adminBulk.usedWhenTheCsv` | (used when the CSV row has no reason) | (utilisé lorsque la ligne CSV n'indique aucun motif) | **41% longer than English** — check it does not overflow. |
| `adminBulk.validInvalid` | {valid} valid, {invalid} invalid | {valid} valides, {invalid} invalides |  |
| `adminConfigReload.apply` | Apply | Appliquer |  |
| `adminConfigReload.applying` | Applying… | Application… |  |
| `adminConfigReload.changeHistory` | Change History | Historique des modifications |  |
| `adminConfigReload.clearBanner` | Clear Banner | Effacer la bannière |  |
| `adminConfigReload.clearing` | Clearing… | Effacement… |  |
| `adminConfigReload.configEditor` | Config Editor | Éditeur de configuration |  |
| `adminConfigReload.configReload` | Config Reload | Rechargement de la configuration |  |
| `adminConfigReload.configTomlContent` | Config TOML content | Contenu du config TOML |  |
| `adminConfigReload.configurationWarnings` | Configuration warnings ({count}) | Avertissements de configuration ({count}) | **28% longer than English** — check it does not overflow. |
| `adminConfigReload.createPendingReload` | Create Pending Reload | Créer un rechargement en attente | **52% longer than English** — check it does not overflow. |
| `adminConfigReload.discard` | Discard | Abandonner |  |
| `adminConfigReload.discarding` | Discarding… | Abandon… |  |
| `adminConfigReload.expiresIn` | Expires in: | Expire dans : |  |
| `adminConfigReload.forceReloadNow` | Force Reload Now | Forcer le rechargement |  |
| `adminConfigReload.globalBanner` | Global Banner | Bandeau global |  |
| `adminConfigReload.hotReloadDisabled` | Hot reload is disabled on this instance ({flag}). Config changes require a server restart. | Le rechargement à chaud est désactivé sur cette instance ({flag}). Toute modification de configuration nécessite un redémarrage du serveur. | **54% longer than English** — check it does not overflow. |
| `adminConfigReload.limitsChanged` | limits changed | limites modifiées |  |
| `adminConfigReload.loading` | Loading… | Chargement… |  |
| `adminConfigReload.maintenanceInProgress` | Maintenance in progress… | Maintenance en cours… |  |
| `adminConfigReload.noBannerCurrentlySet` | No banner currently set. | Aucun bandeau défini actuellement. | **42% longer than English** — check it does not overflow. |
| `adminConfigReload.noChangesRecordedYet` | No changes recorded yet. | Aucune modification enregistrée. | **33% longer than English** — check it does not overflow. |
| `adminConfigReload.noPendingReloadThe` | No pending reload. The file watcher will populate this when a config change is detected. | Aucun rechargement en attente. Le watcher de fichiers remplira cette section dès qu'un changement de configuration sera détecté. | **45% longer than English** — check it does not overflow. |
| `adminConfigReload.pendingReload` | Pending Reload | Rechargement en attente |  |
| `adminConfigReload.reReadsTheConfig` | Re-reads the config file, validates it, and applies it immediately — no confirmation step. | Relit le fichier de configuration, le valide et l'applique immédiatement — sans étape de confirmation. |  |
| `adminConfigReload.reloadFromDisk` | Reload from Disk | Recharger depuis le disque |  |
| `adminConfigReload.reloadNow` | Reload Now | Recharger maintenant |  |
| `adminConfigReload.reloading` | Reloading… | Rechargement… |  |
| `adminConfigReload.setBanner` | Set Banner | Définir la bannière |  |
| `adminConfigReload.setBy` | — set by {user} | — défini par {user} |  |
| `adminConfigReload.setting` | Setting… | Définition… |  |
| `adminConfigReload.validWithWarnings` | This config is valid but raises {count} warning(s): | Cette configuration est valide mais soulève {count} avertissement(s) : | **37% longer than English** — check it does not overflow. |
| `adminConfigReload.validate` | Validate | Valider |  |
| `adminConfigReload.validating` | Validating… | Validation… |  |
| `adminDashboard.cacheSummary` | {rate} of {total} artifact downloads were served from cache since this process started {since}, holding {size} of artifacts. | {rate} des {total} téléchargements d'artefacts ont été servis depuis le cache depuis le démarrage de ce processus le {since}, pour {size} d'artefacts conservés. | **29% longer than English** — check it does not overflow. |
| `adminDashboard.hitRate` | Hit rate | Taux de hit |  |
| `adminDashboard.noRegistriesConfiguredYet` | No registries configured yet | Aucun registre configuré |  |
| `adminDashboard.nothingIsCachedOrServed` | Nothing is cached or served until a registry exists. Add a [[registries]] block to config.toml and reload. | Rien n'est mis en cache ni servi tant qu'aucun registre n'existe. Ajoutez un bloc [[registries]] à config.toml puis rechargez. |  |
| `adminDashboard.openConfig` | Open config | Ouvrir la configuration |  |
| `adminDashboard.openHealth` | Open health | Ouvrir l'état de santé |  |
| `adminDashboard.perRegistry` | Per registry | Par registre |  |
| `adminExploreCache.10Min` | 10 min | 10 min |  |
| `adminExploreCache.cacheIsAlsoInvalidated` | Cache is also invalidated automatically when a package is published to this registry. | Le cache est également invalidé automatiquement lorsqu'un paquet est publié dans ce registre. |  |
| `adminExploreCache.cachesQueryResultsFor` | The package explorer caches database query results for | L'explorateur de paquets met en cache les résultats des requêtes en base pendant | **48% longer than English** — check it does not overflow. |
| `adminExploreCache.clearsOnlyTheEntries` | Clears only the entries belonging to one registry. Use this after a manual data fix or           forced re-index without triggering a full publish. | N'efface que les entrées d'un seul registre. À utiliser après une correction manuelle des données ou une réindexation forcée, sans déclencher une publication complète. |  |
| `adminExploreCache.exploreCache` | Explore Cache | Cache d'exploration |  |
| `adminExploreCache.forcesEveryExploreEndpoint` | Forces every explore endpoint to re-query the database on the next request. Use after bulk           data imports or registry restructuring. | Force chaque endpoint d'exploration à réinterroger la base de données à la prochaine requête. À utiliser après un import de données en masse ou une restructuration des registres. | **27% longer than English** — check it does not overflow. |
| `adminExploreCache.howTheCacheWorks` | How the Cache Works | Fonctionnement du cache |  |
| `adminExploreCache.invalidateAllRegistries` | Invalidate All Registries | Invalider tous les registres |  |
| `adminExploreCache.invalidateByRegistry` | Invalidate by Registry | Invalider par registre |  |
| `adminExploreCache.invalidateEntireCache` | Invalidate Entire Cache | Invalider tout le cache |  |
| `adminExploreCache.invalidateRegistry` | Invalidate Registry | Invalider le registre |  |
| `adminExploreCache.invalidating` | Invalidating… | Invalidation… |  |
| `adminExploreCache.publishingAPackageInvalidates` | Publishing a package invalidates all cached entries for that registry automatically. | Publier un paquet invalide automatiquement toutes les entrées en cache de ce registre. |  |
| `adminExploreCache.registryName` | Registry name | Nom du registre |  |
| `adminExploreCache.resultsAreCachedPer` | Results are cached per query (registry filter, search term, sort, page). | Les résultats sont mis en cache par requête (filtre de registre, terme de recherche, tri, page). | **33% longer than English** — check it does not overflow. |
| `adminExploreCache.theCacheIsIn` | The cache is in-memory and per-instance — a server restart or horizontal scale-out             starts with an empty cache. | Le cache est en mémoire et propre à chaque instance — un redémarrage du serveur ou une montée en charge horizontale démarre avec un cache vide. |  |
| `adminExploreCache.theCacheRepopulatesAutomatically` | The cache repopulates automatically on the next request — no downtime. | Le cache se reconstitue automatiquement à la requête suivante — sans interruption de service. | **33% longer than English** — check it does not overflow. |
| `adminExploreCache.toAvoidExpensiveScans` | to avoid expensive scans on large registries. Stale entries are kept and served if the database becomes unreachable. | pour éviter des analyses coûteuses sur les grands registres. Les entrées périmées sont conservées et servies si la base de données devient injoignable. | **30% longer than English** — check it does not overflow. |
| `adminExploreCache.ttlIs10Minutes` | TTL is 10 minutes. Expired entries are served stale if the database is unreachable. | Le TTL est de 10 minutes. Les entrées expirées sont servies périmées si la base de données est injoignable. | **29% longer than English** — check it does not overflow. |
| `adminExploreCache.upstreamUnavailableFlag` | When the database is unreachable and no cached entry exists, the response includes {flag} so the UI can display a warning. | Lorsque la base de données est injoignable et qu'aucune entrée n'est en cache, la réponse contient {flag} afin que l'interface puisse afficher un avertissement. | **31% longer than English** — check it does not overflow. |
| `adminExploreCache.upstream_unavailableTrue` | upstream_unavailable: true | upstream_unavailable: true | **Kept verbatim.** It is a literal API response field, reproduced so an operator can match it against a log line. |
| `adminHealth.allCachedArtifactsFor` | All cached artifacts for this registry will be permanently removed. Packages will be       re-fetched from upstream on the next request. | Tous les artefacts en cache de ce registre seront définitivement supprimés. Les paquets seront de nouveau récupérés depuis l'upstream à la prochaine requête. |  |
| `adminHealth.artifactRequests` | artifact requests | requêtes d'artefacts |  |
| `adminHealth.cacheHitRate` | Cache hit rate | Taux de hit du cache |  |
| `adminHealth.cacheHits` | Cache hits | Hits de cache |  |
| `adminHealth.cacheMisses` | Cache misses | Miss de cache |  |
| `adminHealth.cachePerformanceSinceLast` | Cache performance since last restart | Performance du cache depuis le dernier redémarrage | **39% longer than English** — check it does not overflow. |
| `adminHealth.cacheSize` | Cache size | Taille du cache |  |
| `adminHealth.clearCache` | Clear Cache | Vider le cache |  |
| `adminHealth.clearCacheFor` | Clear cache for {registry}? | Vider le cache de {registry} ? |  |
| `adminHealth.errorsIn24h` | {count} error in 24 h \| {count} errors in 24 h | {count} erreur en 24 h \| {count} erreurs en 24 h |  |
| `adminHealth.fetchedFromUpstream` | fetched from upstream | récupérés depuis l'upstream | **29% longer than English** — check it does not overflow. |
| `adminHealth.inStorage` | in storage | en stockage |  |
| `adminHealth.lastPull` | Last pull | Dernier pull |  |
| `adminHealth.liveSnapshotOfEachRegistry` | Live snapshot of each registry — packages, cache, pull rates, and recent errors. | Aperçu en direct de chaque registre — paquets, cache, taux de pull et erreurs récentes. |  |
| `adminHealth.noAccessConfigured` | No access configured | Aucun accès configuré |  |
| `adminHealth.noErrors24h` | No errors in the last 24 h | Aucune erreur sur les dernières 24 h | **38% longer than English** — check it does not overflow. |
| `adminHealth.pullsDay` | Pulls / day | Pulls / jour |  |
| `adminHealth.pullsHour` | Pulls / hour | Pulls / heure |  |
| `adminHealth.refreshing` | Refreshing… | Actualisation… |  |
| `adminHealth.registryHealth` | Registry Health | État de santé des registres |  |
| `adminHealth.restrictedNoPublicAccess` | ⚠ Restricted — no public access | ⚠ Restreint — aucun accès public |  |
| `adminHealth.servedFromCache` | served from cache | servis depuis le cache |  |
| `adminHealth.totalCached` | Total cached | Total en cache |  |
| `adminHealth.upstreamError` | Upstream error | Erreur upstream |  |
| `adminHealth.whoHasAccess` | Who has access | Qui y a accès |  |
| `adminIpBlocks.blockIp` | Block IP | Bloquer l'IP |  |
| `adminIpBlocks.blockIpAddress` | Block IP address | Bloquer une adresse IP |  |
| `adminIpBlocks.blockedAt` | Blocked at | Bloquée le |  |
| `adminIpBlocks.blocking` | Blocking… | Blocage… |  |
| `adminIpBlocks.currentlyBlockedIps` | Currently blocked IPs | IP actuellement bloquées |  |
| `adminIpBlocks.default3600S1` | Default: 3600 s (1 hour) | Par défaut : 3600 s (1 heure) |  |
| `adminIpBlocks.durationSeconds` | Duration (seconds) | Durée (secondes) |  |
| `adminIpBlocks.ipAddress` | IP address | Adresse IP |  |
| `adminIpBlocks.ipBlocks` | IP Blocks | Blocages d'IP |  |
| `adminIpBlocks.loading` | Loading… | Chargement… |  |
| `adminIpBlocks.manageManuallyBlockedIpAddresses` | Manage manually blocked IP addresses. Blocked IPs receive 403 responses on all requests. | Gérer les adresses IP bloquées manuellement. Les IP bloquées reçoivent une réponse 403 sur toutes les requêtes. | **26% longer than English** — check it does not overflow. |
| `adminIpBlocks.noIpsAreCurrently` | No IPs are currently blocked. | Aucune IP n'est actuellement bloquée. | **28% longer than English** — check it does not overflow. |
| `adminIpBlocks.optionalReason` | Optional reason | Motif facultatif |  |
| `adminIpBlocks.theIpWillBe` | The IP will be blocked for the specified duration and receive 403 on all requests. | L'IP sera bloquée pendant la durée indiquée et recevra un 403 sur toutes ses requêtes. |  |
| `adminIpBlocks.thisIpWillBe` | This IP will be immediately allowed to send requests again. | Cette IP pourra de nouveau envoyer des requêtes immédiatement. |  |
| `adminIpBlocks.unblocking` | Unblocking… | Déblocage… |  |
| `adminIpBlocks.unblocksAt` | Unblocks at | Débloquée le |  |
| `adminNav.accessCheck` | Access Check | Vérification d'accès |  |
| `adminNav.allPackages` | All Packages | Tous les paquets |  |
| `adminNav.auditLog` | Audit Log | Journal d'audit |  |
| `adminNav.betaChannel` | Beta Channel | Canal bêta |  |
| `adminNav.bulkBlock` | Bulk Block | Blocage de masse |  |
| `adminNav.configReload` | Config Reload | Rechargement de config |  |
| `adminNav.dashboard` | Dashboard | Tableau de bord |  |
| `adminNav.exploreCache` | Explore Cache | Explorer le cache |  |
| `adminNav.health` | Health | Santé |  |
| `adminNav.ipBlocks` | IP Blocks | Blocages IP |  |
| `adminNav.namespacesChannels` | Namespaces & Channels | Namespaces et canaux |  |
| `adminNav.notifications` | Notifications | Notifications |  |
| `adminNav.observability` | Observability | Observabilité |  |
| `adminNav.operations` | Operations | Opérations |  |
| `adminNav.packages` | Packages | Paquets |  |
| `adminNav.sbomExport` | SBOM Export | Export SBOM |  |
| `adminNav.securityAccess` | Security & Access | Sécurité et accès |  |
| `adminNav.teamNamespaces` | Team Namespaces | Namespaces d'équipe |  |
| `adminNav.users` | Users | Utilisateurs |  |
| `adminNav.warming` | Warming | Préchauffage | **Decided: `Préchauffage`.** The most debatable row on this sheet: `warming` is cache jargon and stays English in a lot of FR infra writing. Translated because it names a *page*, and because the operator reading it may not be the one who configured it. Applied consistently across `adminWarming.*`. |
| `adminNotifications.channels` | Channels | Canaux |  |
| `adminNotifications.channelsDefinedIn` | Channels are defined in {file} under {block}. URLs and secrets are not displayed here. | Les canaux sont définis dans {file}, sous {block}. Les URL et les secrets ne sont pas affichés ici. |  |
| `adminNotifications.configuredChannels` | Configured Channels | Canaux configurés |  |
| `adminNotifications.create` | Create | Créer |  |
| `adminNotifications.deleteSubscription` | Delete subscription? | Supprimer cet abonnement ? | **30% longer than English** — check it does not overflow. |
| `adminNotifications.editSubscription` | Edit Subscription | Modifier l'abonnement |  |
| `adminNotifications.eventTypes` | Event types | Types d'événements |  |
| `adminNotifications.inboundEvents` | Inbound Events | Événements entrants |  |
| `adminNotifications.leaveBlankForAll` | (leave blank for all) | (laisser vide pour tous) |  |
| `adminNotifications.manageOutboundNotificationSubscriptionsAnd` | Manage outbound notification subscriptions and monitor inbound webhook events. | Gérer les abonnements de notification sortants et suivre les événements webhook entrants. |  |
| `adminNotifications.newSubscription` | New Subscription | Nouvel abonnement |  |
| `adminNotifications.noChannelsConfigured` | No channels configured. Add {block} entries to config.toml. | Aucun canal configuré. Ajoutez des entrées {block} dans config.toml. |  |
| `adminNotifications.noInboundEventsReceived` | No inbound events received yet. | Aucun événement entrant reçu. |  |
| `adminNotifications.noSubscriptionsConfigured` | No subscriptions configured. | Aucun abonnement configuré. |  |
| `adminNotifications.packageName` | Package name | Nom du paquet |  |
| `adminNotifications.receivedAt` | Received at | Reçu le |  |
| `adminNotifications.sourceIp` | Source IP | IP source |  |
| `adminNotifications.subscriptions` | Subscriptions | Abonnements |  |
| `adminNotifications.thisActionCannotBeUndone` | This action cannot be undone. | Cette action est irréversible. |  |
| `adminNotifications.update` | Update | Mettre à jour |  |
| `adminNotifications.webhooksNotifications` | Webhooks & Notifications | Webhooks et notifications |  |
| `adminPackages.allPackages` | All packages | Tous les paquets |  |
| `adminPackages.blockAPackage` | Block a package | Bloquer un paquet |  |
| `adminPackages.blockNewPackage` | Block new package | Bloquer un nouveau paquet |  |
| `adminPackages.blockPackage` | Block package | Bloquer le paquet |  |
| `adminPackages.blockSelected` | Block selected | Bloquer la sélection |  |
| `adminPackages.deleteSelected` | Delete selected | Supprimer la sélection |  |
| `adminPackages.filterByNameRegistry` | Filter by name, registry, or version… | Filtrer par nom, registre ou version… |  |
| `adminPackages.filterPackages` | Filter packages | Filtrer les paquets |  |
| `adminPackages.lastPulled` | Last pulled | Dernier pull |  |
| `adminPackages.lastPulledBy` | Last pulled by | Dernier pull par |  |
| `adminPackages.noPackagesMatchYourFilter` | No packages match your filter. | Aucun paquet ne correspond à votre filtre. | **40% longer than English** — check it does not overflow. |
| `adminPackages.noPackagesYet` | No packages yet. | Aucun paquet pour l'instant. |  |
| `adminPackages.optional` | (optional) | (facultatif) |  |
| `adminPackages.ownerRepoOrLodash` | owner/repo or lodash or serde | owner/repo ou lodash ou serde |  |
| `adminPackages.preEmptivelyBlockA` | Pre-emptively block a package before it is downloaded. The block takes effect immediately           — any subsequent request for that package will be denied. | Bloquer un paquet de manière préventive, avant tout téléchargement. Le blocage prend effet immédiatement — toute requête ultérieure pour ce paquet sera refusée. |  |
| `adminPackages.selectAllPackages` | Select all packages | Sélectionner tous les paquets |  |
| `adminPackages.selectPackage` | Select {name} | Sélectionner {name} |  |
| `adminPackages.tarball123456789Download` | tarball / 123456789 / download | tarball / 123456789 / download |  |
| `adminPackages.tryClearingTheFilter` | Try clearing the filter. | Essayez d'effacer le filtre. |  |
| `adminPackages.unblockSelected` | Unblock selected | Débloquer la sélection |  |
| `adminPackages.versionTag` | Version / tag | Version / tag |  |
| `adminSbom.aboutSbomFormats` | About SBOM Formats | À propos des formats SBOM |  |
| `adminSbom.catalog` | catalog | catalogue |  |
| `adminSbom.cyclonedx14` | CycloneDX 1.4 | CycloneDX 1.4 |  |
| `adminSbom.cyclonedxBlurb` | {name} — OWASP standard optimised for security tooling. Preferred for vulnerability scanning and SBOM-driven dependency analysis. | {name} — norme OWASP optimisée pour l'outillage de sécurité. À privilégier pour l'analyse de vulnérabilités et l'analyse de dépendances pilotée par SBOM. |  |
| `adminSbom.downloadSbom` | Download SBOM | Télécharger le SBOM |  |
| `adminSbom.exportOrgLevelSbom` | Export Org-Level SBOM | Exporter le SBOM global |  |
| `adminSbom.exporting` | Exporting… | Export… |  |
| `adminSbom.generatesAMergedSbom` | Generates a merged SBOM covering all artifacts served in the selected time window. Leave           filters empty to export everything. | Génère un SBOM fusionné couvrant tous les artefacts servis sur la période sélectionnée. Laissez les filtres vides pour tout exporter. |  |
| `adminSbom.optional` | (optional) | (facultatif) |  |
| `adminSbom.perArtifactSboms` | Per-artifact SBOMs (SPDX or CycloneDX) are also available from the {catalog} version detail view. | Des SBOM par artefact (SPDX ou CycloneDX) sont également disponibles depuis la vue détaillée d'une version dans le {catalog}. | **29% longer than English** — check it does not overflow. |
| `adminSbom.sbomExport` | SBOM Export | Export SBOM |  |
| `adminSbom.spdx23` | SPDX 2.3 | SPDX 2.3 |  |
| `adminSbom.spdxBlurb` | {name} — ISO/IEC standard widely used for compliance and license tracking. Preferred for legal review and OpenChain-conformant workflows. | {name} — norme ISO/IEC largement utilisée pour la conformité et le suivi des licences. À privilégier pour la revue juridique et les processus conformes à OpenChain. |  |
| `adminTeamNamespaces.assignPackageNamePrefixesTo` | Assign package name prefixes to auth-provider groups to control who may publish within them. | Attribuer des préfixes de nom de paquet aux groupes du fournisseur d'authentification pour contrôler qui peut y publier. | **30% longer than English** — check it does not overflow. |
| `adminTeamNamespaces.claimNamespace` | Claim namespace | Revendiquer le namespace |  |
| `adminTeamNamespaces.claimedBy` | Claimed by | Revendiqué par |  |
| `adminTeamNamespaces.claiming` | Claiming… | Revendication… |  |
| `adminTeamNamespaces.groupId` | Group ID | ID du groupe |  |
| `adminTeamNamespaces.mustMatchTheGroup` | Must match the group name in your auth provider's claims. | Doit correspondre au nom du groupe dans les claims de votre fournisseur. | **26% longer than English** — check it does not overflow. |
| `adminTeamNamespaces.namespaceClaims` | Namespace claims | Namespaces revendiqués |  |
| `adminTeamNamespaces.noNamespaceClaimsForThis` | No namespace claims for this registry. | Aucune revendication de namespace pour ce registre. | **34% longer than English** — check it does not overflow. |
| `adminTeamNamespaces.optionalYourUserId` | Optional — your user ID | Facultatif — votre ID utilisateur | **43% longer than English** — check it does not overflow. |
| `adminTeamNamespaces.packagesStartingWith` | Packages whose name equals or starts with {prefix} will be restricted. | Les paquets dont le nom est égal à {prefix} ou commence par ce préfixe seront restreints. | **27% longer than English** — check it does not overflow. |
| `adminTeamNamespaces.prefix` | Prefix | Préfixe |  |
| `adminTeamNamespaces.releaseClaim` | Release claim | Libérer la revendication |  |
| `adminTeamNamespaces.releaseExplanation` | The prefix {prefix} will no longer be restricted to group {group}. Any authenticated user will be able to publish packages under this prefix. | Le préfixe {prefix} ne sera plus réservé au groupe {group}. Tout utilisateur authentifié pourra publier des paquets sous ce préfixe. |  |
| `adminTeamNamespaces.releaseNamespaceClaim` | Release namespace claim? | Libérer ce namespace ? |  |
| `adminTeamNamespaces.releasing` | Releasing… | Libération… |  |
| `adminTeamNamespaces.restrictPublishingIn` | Restrict publishing under a prefix in {registry} to a specific group. | Restreindre la publication sous un préfixe dans {registry} à un groupe précis. |  |
| `adminTeamNamespaces.selectARegistry` | Select a registry… | Choisir un registre… |  |
| `adminTeamNamespaces.selectARegistryToView` | Select a registry to view namespace claims. | Choisissez un registre pour voir les revendications. |  |
| `adminTeamNamespaces.teamNamespaces` | Team Namespaces | Namespaces d'équipe |  |
| `adminUsers.blockUser` | Block User | Bloquer l'utilisateur |  |
| `adminUsers.blockUser2` | Block user | Bloquer un utilisateur |  |
| `adminUsers.blockUserAccountsBlockedUsers` | Block user accounts. Blocked users receive 401 responses on all authenticated requests. | Bloquer des comptes utilisateur. Les utilisateurs bloqués reçoivent une réponse 401 sur toutes les requêtes authentifiées. | **40% longer than English** — check it does not overflow. |
| `adminUsers.blockedAt` | Blocked at | Bloqué le |  |
| `adminUsers.blockedBy` | Blocked by | Bloqué par |  |
| `adminUsers.currentlyBlockedUsers` | Currently blocked users | Utilisateurs actuellement bloqués | **43% longer than English** — check it does not overflow. |
| `adminUsers.loading` | Loading… | Chargement… |  |
| `adminUsers.noUsersAreCurrently` | No users are currently blocked. | Aucun utilisateur n'est actuellement bloqué. | **42% longer than English** — check it does not overflow. |
| `adminUsers.optionalReason` | Optional reason | Motif facultatif |  |
| `adminUsers.theUserWillReceive` | The user will receive 401 on all authenticated requests until unblocked. | L'utilisateur recevra un 401 sur toutes ses requêtes authentifiées jusqu'à son déblocage. |  |
| `adminUsers.thisUserWillBe` | This user will be immediately allowed to authenticate again. | Cet utilisateur pourra de nouveau s'authentifier immédiatement. |  |
| `adminUsers.userBlocks` | User Blocks | Blocages d'utilisateurs |  |
| `adminUsers.userId` | User ID | Identifiant utilisateur |  |
| `adminWarming.cacheWarming` | Cache Warming | Préchauffage du cache |  |
| `adminWarming.commaSeparatedForPath` | Comma-separated. For path-addressed registries. | Séparés par des virgules. Pour les registres adressés par chemin. | **38% longer than English** — check it does not overflow. |
| `adminWarming.commaSeparatedOmitVersion` | Comma-separated. Omit version to warm latest_n. | Séparés par des virgules. Omettez la version pour préchauffer latest_n. | **51% longer than English** — check it does not overflow. |
| `adminWarming.deleteCachedArtifact` | Delete Cached Artifact | Supprimer l'artefact en cache | **32% longer than English** — check it does not overflow. |
| `adminWarming.deleteFromCache` | Delete from Cache | Retirer du cache |  |
| `adminWarming.deleted` | Deleted | Supprimé |  |
| `adminWarming.deleting` | Deleting… | Suppression… |  |
| `adminWarming.lodashReact180` | lodash, react{'@'}18.0.0 | lodash, react{'@'}18.0.0 |  |
| `adminWarming.noWarmingConfigured` | No registries have warming configured. Add {packages} or {paths} to a registry in your config. | Aucun registre n'a de préchauffage configuré. Ajoutez {packages} ou {paths} à un registre dans votre configuration. |  |
| `adminWarming.notCachedNothingToRemove` | Not cached — nothing to remove | Non mis en cache — rien à retirer |  |
| `adminWarming.packageMode` | Package | Paquet |  |
| `adminWarming.pathMode` | Path | Chemin |  |
| `adminWarming.registriesWithWarmingConfiguredTrigger` | Registries with warming configured. Trigger a warm run to pre-fetch artifacts into the local cache. | Registres avec préchauffage configuré. Lancez un run pour pré-charger les artefacts dans le cache local. |  |
| `adminWarming.removeArtifactHelp` | Remove a single proxy-cached artifact from storage. The next request will re-download it from upstream. Use {path} mode for path-addressed registries (jetbrains/deb/rpm); use {package} mode for all others. | Retire du stockage un seul artefact mis en cache par le proxy. La requête suivante le retéléchargera depuis l'upstream. Utilisez le mode {path} pour les registres adressés par chemin (jetbrains/deb/rpm) ; utilisez le mode {package} pour tous les autres. |  |
| `adminWarming.warmNow` | Warm Now | Préchauffer maintenant |  |
| `adminWarming.warming` | Warming… | Préchauffage… |  |
| `appFooter.reportABug` | Report a bug | Signaler un bug |  |
| `appFooter.reportASecurityIssue` | Report a security issue | Signaler un problème de sécurité | **39% longer than English** — check it does not overflow. |
| `appFooter.version` | BatleHub v{version} | BatleHub v{version} |  |
| `appHeader.myNamespace` | My Namespace | Mon namespace |  |
| `appHeader.myProfile` | My Profile | Mon profil |  |
| `appHeader.myTokens` | My Tokens | Mes tokens |  |
| `appHeader.signOut` | Sign out | Se déconnecter |  |
| `asyncState.loading` | Loading… | Chargement… |  |
| `auditLog.allActions` | All actions | Toutes les actions |  |
| `auditLog.auditLog` | Audit Log | Journal d'audit |  |
| `auditLog.export` | Export | Exporter |  |
| `auditLog.exportFormat` | Export format | Format d'export |  |
| `auditLog.filterByAction` | Filter by action | Filtrer par action |  |
| `auditLog.filterByUser` | Filter by user… | Filtrer par utilisateur… |  |
| `auditLog.filterByUser2` | Filter by user | Filtrer par utilisateur |  |
| `auditLog.loading` | Loading… | Chargement… |  |
| `auditLog.noEventsMatchTheCurrent` | No events match the current filters. | Aucun événement ne correspond aux filtres actuels. | **39% longer than English** — check it does not overflow. |
| `catalog.allRegistries` | All registries ({count}) | Tous les registres ({count}) |  |
| `catalog.clearSearch` | Clear search | Effacer la recherche |  |
| `catalog.emptyBody` | Packages appear here once something pulls them through this instance, or once they are published to it. | Les paquets apparaissent ici dès qu'ils transitent par cette instance, ou dès qu'ils y sont publiés. |  |
| `catalog.emptyFilteredBody` | No package in this view matches. Upstream results appear here too when the registry supports search. | Aucun paquet de cette vue ne correspond. Les résultats upstream apparaissent également ici lorsque le registre gère la recherche. | **29% longer than English** — check it does not overflow. |
| `catalog.emptyFilteredTitle` | Nothing matches that search | Aucun résultat pour cette recherche | **30% longer than English** — check it does not overflow. |
| `catalog.emptyTitle` | No packages cached yet | Aucun paquet en cache |  |
| `catalog.registries` | Registries | Registres |  |
| `catalog.upstream` | upstream | upstream | **Decided: kept as `upstream`.** Widely used untranslated by FR infra teams, and it matches the config key `upstreams` that operators edit. Applied consistently in `catalog.emptyFilteredBody` too. |
| `cliDownload.buildFromSource` | Build from source | Compiler depuis les sources |  |
| `cliDownload.checkIdentity` | Check identity | Vérifier l'identité |  |
| `cliDownload.commonCommandsToGet` | Common commands to get started. | Commandes courantes pour démarrer. |  |
| `cliDownload.createConfigOrRun` | Create {path} or run {command}. | Créez {path} ou lancez {command}. |  |
| `cliDownload.downloadAndConfigure` | Download and configure {cli} | Télécharger et configurer {cli} |  |
| `cliDownload.downloadBatlehubCli` | Download batlehub-cli | Télécharger batlehub-cli |  |
| `cliDownload.downloading` | Downloading… | Téléchargement… |  |
| `cliDownload.fromThisServer` | From this server | Depuis ce serveur |  |
| `cliDownload.getThePreBuilt` | Get the pre-built binary served by this server, or build from source. | Récupérez le binaire précompilé servi par ce serveur, ou compilez depuis les sources. |  |
| `cliDownload.listPackages` | List packages | Lister les paquets |  |
| `cliDownload.listRegistries` | List registries | Lister les registres |  |
| `cliDownload.overrideWithEnv` | Override any setting with environment variables: | Surchargez n'importe quel réglage via les variables d'environnement : | **44% longer than English** — check it does not overflow. |
| `cliDownload.publish` | Publish | Publier |  |
| `cliDownload.quickReference` | Quick reference | Référence rapide |  |
| `cliDownload.yankVersion` | Yank version | Yank d'une version | **`Yank` kept verbatim inside a French sentence** — it is the CLI subcommand (`version yank`). Translating the verb would leave the reader unable to find the command. |
| `common.action` | Action | Action |  |
| `common.actions` | Actions | Actions |  |
| `common.admin` | Admin | Admin | **Kept as `Admin`.** Used identically in French, and it labels a section whose URL is `/admin`. |
| `common.administration` | Administration | Administration |  |
| `common.artifact` | Artifact | Artefact |  |
| `common.block` | Block | Bloquer |  |
| `common.blocked` | Blocked | Bloqué |  |
| `common.cached` | Cached | En cache |  |
| `common.cancel` | Cancel | Annuler |  |
| `common.channel` | Channel | Canal |  |
| `common.clear` | Clear filter | Effacer le filtre |  |
| `common.clearAction` | Clear | Effacer |  |
| `common.close` | Close | Fermer |  |
| `common.component` | Component | Composant |  |
| `common.configuration` | Configuration | Configuration |  |
| `common.copied` | Copied! | Copié ! | French convention puts a space before `!` — `Copié !`. Intentional, not a typo. |
| `common.copy` | Copy | Copier |  |
| `common.created` | Created | Créé |  |
| `common.date` | Date | Date |  |
| `common.delete` | Delete | Supprimer |  |
| `common.details` | Details | Détails |  |
| `common.dialog` | Dialog | Dialogue |  |
| `common.dismiss` | Dismiss | Ignorer |  |
| `common.distribution` | Distribution | Distribution |  |
| `common.docs` | Docs | Docs |  |
| `common.documentation` | Documentation | Documentation |  |
| `common.download` | Download | Télécharger |  |
| `common.downloads` | Downloads | Téléchargements |  |
| `common.edit` | Edit | Modifier |  |
| `common.enabled` | Enabled | Activé |  |
| `common.error` | Error | Erreur |  |
| `common.errors` | Errors | Erreurs |  |
| `common.events` | Events | Événements |  |
| `common.expired` | Expired | Expiré |  |
| `common.expires` | Expires | Expire |  |
| `common.file` | File | Fichier |  |
| `common.firewall` | Firewall | Firewall | **Kept verbatim.** `Pare-feu` exists but `firewall` is what the surrounding infra vocabulary uses. |
| `common.format` | Format | Format |  |
| `common.from` | From | De |  |
| `common.group` | Group | Groupe |  |
| `common.groups` | Groups | Groupes |  |
| `common.hits` | Hits | Hits | **Kept verbatim.** Cache vocabulary, used untranslated by FR infra teams, and it sits in a column beside `Misses`. |
| `common.identity` | Identity | Identité |  |
| `common.info` | Info | Info |  |
| `common.invalid` | Invalid | Invalide |  |
| `common.level` | Level | Niveau |  |
| `common.lifetime` | Lifetime | Durée de vie |  |
| `common.loading` | Loading… | Chargement… |  |
| `common.members` | Members | Membres |  |
| `common.message` | Message | Message |  |
| `common.misses` | Misses | Misses | **Kept verbatim**, same reason as `common.hits`. |
| `common.name` | Name | Nom |  |
| `common.next` | Next | Suivant |  |
| `common.outcome` | Outcome | Résultat | **Decided: `Résultat`**, the same word as `common.result`. `Issue` was tried first for being shorter, but it collides with the ticket sense and reads wrong in an audit table. Two English words legitimately map to one French word here. |
| `common.package` | Package | Paquet |  |
| `common.packages` | Packages | Paquets |  |
| `common.page` | Page | Page |  |
| `common.path` | Path | Chemin |  |
| `common.paths` | Paths | Chemins |  |
| `common.prefix` | Prefix | Préfixe |  |
| `common.preview` | Preview | Aperçu |  |
| `common.previous` | Previous | Précédent |  |
| `common.proxied` | Proxied | Via proxy | **Decided: `Via proxy`.** A participle would have to be `proxifié`, which is not established French; the prepositional form says the same thing without inventing a verb. |
| `common.proxy` | Proxy | Proxy |  |
| `common.published` | Published | Publié |  |
| `common.reason` | Reason | Motif |  |
| `common.refresh` | Refresh | Actualiser |  |
| `common.registries` | Registries | Registres |  |
| `common.registry` | Registry | Registre |  |
| `common.release` | Release | Release | **Kept verbatim** — it names the release channel, not the act of releasing. |
| `common.remove` | Remove | Retirer |  |
| `common.reset` | Reset | Réinitialiser |  |
| `common.result` | Result | Résultat |  |
| `common.retry` | Retry | Réessayer |  |
| `common.role` | Role | Rôle |  |
| `common.security` | Security | Sécurité |  |
| `common.signature` | Signature | Signature |  |
| `common.source` | Source | Source |  |
| `common.state` | State | État |  |
| `common.status` | Status | Statut |  |
| `common.storage` | Storage | Stockage |  |
| `common.summary` | Summary | Résumé |  |
| `common.time` | Time | Heure |  |
| `common.type` | Type | Type |  |
| `common.unblock` | Unblock | Débloquer |  |
| `common.upstream` | Upstream | Upstream | **Kept verbatim**, consistent with `catalog.upstream` and the `upstreams` config key. |
| `common.user` | User | Utilisateur |  |
| `common.valid` | Valid | Valide |  |
| `common.version` | Version | Version |  |
| `common.versions` | Versions | Versions |  |
| `common.view` | View | Voir |  |
| `common.visibility` | Visibility | Visibilité |  |
| `common.warning` | Warning | Avertissement |  |
| `common.webhook` | Webhook | Webhook | **Kept verbatim.** It is the config key and the API concept. |
| `common.when` | When | Quand |  |
| `common.you` | You | Vous |  |
| `config.copyToml` | Copy TOML | Copier le TOML |  |
| `config.readOnlyBody` | This instance's config is mounted read-only, so it cannot be edited here. Change it where it is defined — the ConfigMap, Helm values, or the file on the host — and the server picks the change up on its next reload. | La configuration de cette instance est montée en lecture seule ; elle ne peut donc pas être modifiée ici. Modifiez-la là où elle est définie — la ConfigMap, les values Helm, ou le fichier sur l'hôte — et le serveur la prendra en compte au prochain rechargement. | **`ConfigMap`, `Helm` kept verbatim** — product names. Enforced by a test. |
| `config.readOnlyTitle` | Configuration (read-only) | Configuration (lecture seule) |  |
| `dashboard.allAnswering` | All {count} registries are answering. | Les {count} registres répondent. |  |
| `dashboard.freshBody` | Nothing is cached or served until a registry exists. Add a [[registries]] block to config.toml and reload. | Rien n'est mis en cache ni servi tant qu'aucun registre n'existe. Ajoutez un bloc [[registries]] dans config.toml puis rechargez. | Same verbatim rule as above. |
| `dashboard.freshTitle` | No registries configured yet | Aucun registre configuré |  |
| `dashboard.healthUnknown` | Health could not be read. | Impossible de lire l'état de santé. | **40% longer than English** — check it does not overflow. |
| `dashboard.openHealth` | Open health | Ouvrir l'état de santé |  |
| `dashboard.perRegistry` | Per registry | Par registre |  |
| `dashboard.someDegraded` | {failing} of {total} registries reported errors: {names}. | {failing} registres sur {total} ont signalé des erreurs : {names}. |  |
| `dashboard.title` | Dashboard | Tableau de bord |  |
| `destructive.canUndo` | This can be undone afterwards. | Cette action pourra être annulée. |  |
| `destructive.cannotUndo` | This cannot be undone. The artifacts and their metadata are removed permanently. | Cette action est irréversible. Les artefacts et leurs métadonnées sont supprimés définitivement. | **Reviewed and kept.** Safety-critical: this sentence stands between an operator and a permanent purge, and `irréversible` was judged to carry the same weight as the English. |
| `destructive.typeToConfirm` | Type {name} to confirm | Saisissez {name} pour confirmer | `{name}` is injected verbatim — the placeholder must survive, and a test enforces that. **41% longer than English** — check it does not overflow. |
| `home.admin` | Admin | Administration |  |
| `home.browse` | Browse packages | Parcourir les paquets |  |
| `home.configGuide` | Configuration guide | Guide de configuration |  |
| `home.freshBodyAdmin` | Add a [[registries]] block to config.toml, then reload the configuration. Nothing is cached or served until a registry exists. | Ajoutez un bloc [[registries]] dans config.toml, puis rechargez la configuration. Rien n'est mis en cache ni servi tant qu'aucun registre n'existe. | **`[[registries]]` and `config.toml` kept verbatim** — they are literal things to type. Enforced by a test. |
| `home.freshBodyOther` | An administrator has not configured any registries. Until one exists there is nothing to pull or publish. | Aucun registre n'a été configuré par un administrateur. Tant qu'il n'en existe pas, il n'y a rien à télécharger ni à publier. |  |
| `home.freshTitleAdmin` | No registries configured yet | Aucun registre configuré |  |
| `home.freshTitleOther` | This instance has no registries yet | Cette instance n'a encore aucun registre |  |
| `home.myNamespace` | My namespace | Mon namespace | **Kept verbatim** for the same reason as `account.namespace`. |
| `home.openConfig` | Open config | Ouvrir la configuration |  |
| `home.pointTool` | Point a tool at this instance | Configurer un outil pour cette instance | **34% longer than English** — check it does not overflow. |
| `home.greeting` | Hello {user}. | Bonjour {user}. | — |
| `home.statPublishing` | Accepting publishes | Acceptent les publications |  |
| `home.statRegistries` | Registries | Registres |  |
| `home.statYou` | You | Vous |  |
| `home.tagline` | A cache and registry for the packages your builds pull. | Un cache et un registre pour les paquets que vos builds téléchargent. | **25% longer than English** — check it does not overflow. |
| `homePage.aCacheAndRegistry` | A cache and registry for the packages your builds pull. | Un cache et un registre pour les paquets que vos builds téléchargent. | **25% longer than English** — check it does not overflow. |
| `homePage.acceptingPublishes` | Accepting publishes | Acceptent les publications |  |
| `homePage.browsePackages` | Browse packages | Parcourir les paquets |  |
| `homePage.configurationGuide` | Configuration guide | Guide de configuration |  |
| `homePage.myNamespace` | My namespace | Mon namespace |  |
| `homePage.openConfig` | Open config | Ouvrir la configuration |  |
| `homePage.pointAToolAt` | Point a tool at this instance | Configurer un outil pour cette instance | **34% longer than English** — check it does not overflow. |
| `locale.en` | English | English | Language names are conventionally written in their own language, so both stay as-is in both catalogues. |
| `locale.fr` | Français | Français | Same. |
| `locale.label` | Language | Langue |  |
| `locale.system` | System | Système |  |
| `locale.systemNote` | System follows your browser — currently {locale}. | Système suit votre navigateur — actuellement {locale}. |  |
| `loginPage.authenticateToAccessProtected` | Authenticate to access protected resources. | Authentifiez-vous pour accéder aux ressources protégées. | **30% longer than English** — check it does not overflow. |
| `loginPage.bearerToken` | Bearer token | Token Bearer |  |
| `loginPage.continueWithoutSigningIn` | Continue without signing in | Continuer sans se connecter |  |
| `loginPage.orUseAToken` | or use a token | ou utiliser un token |  |
| `loginPage.pasteYourTokenHere` | Paste your token here | Collez votre token ici |  |
| `loginPage.redirecting` | Redirecting… | Redirection… |  |
| `loginPage.signIn` | Sign in | Se connecter |  |
| `loginPage.signInWithOidc` | Sign in with OIDC | Se connecter avec OIDC |  |
| `loginPage.signInWithToken` | Sign in with token | Se connecter avec un token |  |
| `loginPage.signingIn` | Signing in… | Connexion… |  |
| `myNamespace.clickARowTo` | Click a row to browse its packages. | Cliquez sur une ligne pour parcourir ses paquets. | **40% longer than English** — check it does not overflow. |
| `myNamespace.loading` | Loading… | Chargement… |  |
| `myNamespace.myNamespaces` | My namespaces | Mes namespaces |  |
| `myNamespace.publishANewPackage` | Publish a new package to one of your registries. | Publier un nouveau paquet dans l'un de vos registres. |  |
| `myNamespace.publishedVersionsUnderTheSelected` | Published versions under the selected namespace. | Versions publiées sous le namespace sélectionné. |  |
| `myNamespace.selectANamespaceRowAbove` | Select a namespace row above to browse its packages. | Sélectionnez une ligne ci-dessus pour parcourir ses paquets. |  |
| `myNamespace.teamNamespace` | Team Namespace | Namespace d'équipe |  |
| `myNamespace.uploadPackage` | Upload package | Envoyer un paquet |  |
| `myNamespace.viewAndManageThePackages` | View and manage the packages and namespaces owned by your groups. | Voir et gérer les paquets et namespaces détenus par vos groupes. |  |
| `myNamespace.youAreNotA` | You are not a member of any groups. Contact your administrator to be added to a team           namespace. | Vous n'êtes membre d'aucun groupe. Contactez votre administrateur pour être ajouté à un namespace d'équipe. |  |
| `myNamespace.yourGroups` | Your groups | Vos groupes |  |
| `myProfile.authProvider` | Auth provider | Fournisseur d'authentification |  |
| `myProfile.dynamicGroups` | Dynamic groups assigned by your identity provider. Groups with a provider prefix (e.g. {example}) are scoped to that provider; unprefixed values were mapped directly to a role. | Groupes dynamiques attribués par votre fournisseur d'identité. Les groupes préfixés par un fournisseur (par ex. {example}) sont limités à ce fournisseur ; les valeurs sans préfixe ont été associées directement à un rôle. |  |
| `myProfile.groupsArePopulatedWhen` | Groups are populated when you authenticate via an OIDC or Kubernetes provider that             includes group claims. | Les groupes sont renseignés lorsque vous vous authentifiez via un fournisseur OIDC ou Kubernetes qui inclut des claims de groupe. |  |
| `myProfile.noGroupsAssignedTo` | No groups assigned to this session. | Aucun groupe attribué à cette session. |  |
| `myProfile.tokenAnonymous` | Token / anonymous | Token / anonyme | **Merged with a `/`.** The two auth states — a custom/static token, which carries no identity-provider group propagation, and no token at all — read as one alternative in French exactly as in English, so the slash is kept rather than expanded into a sentence. The only row where a slash carries the same meaning in both languages. |
| `myProfile.userId` | User ID | Identifiant utilisateur |  |
| `myProfile.yourCurrentSessionInformation` | Your current session information. | Informations sur votre session actuelle. |  |
| `namespace.noClaimsBody` | None of your groups own a package-name prefix on this instance. An administrator assigns these under Team Namespaces. | Aucun de vos groupes ne possède de préfixe de nom de paquet sur cette instance. Un administrateur les attribue depuis Team Namespaces. |  |
| `namespace.noClaimsTitle` | No namespace claims | Aucun namespace attribué |  |
| `namespace.noPackagesBody` | Packages appear once someone publishes under this namespace prefix. | Les paquets apparaissent dès que quelqu'un publie sous ce préfixe de namespace. |  |
| `namespace.noPackagesTitle` | Nothing published here yet | Rien n'est encore publié ici |  |
| `namespacePackagesTable.editVisibility` | Edit visibility | Modifier la visibilité |  |
| `namespacePackagesTable.loading` | Loading… | Chargement… |  |
| `namespacePackagesTable.packageVisibility` | Package visibility | Visibilité du paquet |  |
| `namespacePackagesTable.publishedBy` | Published by | Publié par |  |
| `namespacePackagesTable.yanked` | (yanked) | (yanked) |  |
| `namespaceUpload.cliInstructions` | CLI instructions | Instructions CLI |  |
| `namespaceUpload.extensionId` | Extension ID | ID d'extension |  |
| `namespaceUpload.fileUpload` | File upload | Envoi de fichier |  |
| `namespaceUpload.modulePath` | Module path | Chemin du module |  |
| `namespaceUpload.noRegistriesInLocal` | No registries in Local or Hybrid mode are configured. | Aucun registre en mode local ou hybrid n'est configuré. |  |
| `namespaceUpload.publishedSuccessfully` | Published successfully. | Publié avec succès. |  |
| `namespaceUpload.publisherName` | (publisher.name) | (publisher.name) |  |
| `namespaceUpload.selectRegistry` | Select registry… | Choisir un registre… |  |
| `namespaceUpload.suite` | (suite) | (suite) |  |
| `namespaceUpload.upload` | Upload | Téléverser |  |
| `namespaceUpload.uploading` | Uploading… | Téléversement… |  |
| `nav.admin` | Admin | Administration |  |
| `nav.docs` | Docs | Documentation |  |
| `nav.packages` | Packages | Paquets | **Decided: `Paquets`.** Standard FR dev usage; reads as French rather than as a half-translated interface, despite the URL staying `/packages`. |
| `nav.setup` | Setup | Configuration |  |
| `nav.signOut` | Sign out | Se déconnecter |  |
| `packageBetaChannel.betaChannelAccess` | Beta Channel Access | Accès au canal bêta |  |
| `packageBetaChannel.grantedBy` | Granted by | Accordé par |  |
| `packageBetaChannel.hide` | ▲ hide | ▲ masquer |  |
| `packageBetaChannel.noBetaChannelMembers` | No beta channel members — pre-release versions are not accessible to anyone. | Aucun membre du canal bêta — les versions pre-release ne sont accessibles à personne. |  |
| `packageBetaChannel.preReleaseVersionsAre` | Pre-release versions are only accessible to the users and groups listed here. | Les versions pre-release ne sont accessibles qu'aux utilisateurs et groupes listés ici. |  |
| `packageBetaChannel.principalId` | Principal ID | Identifiant du principal |  |
| `packageBetaChannel.show` | ▼ show | ▼ afficher |  |
| `packageCatalog.allRegistries` | All registries ({count}) | Tous les registres ({count}) |  |
| `packageCatalog.cachedPackagesTotal` | {count} cached package total \| {count} cached packages total | {count} paquet en cache au total \| {count} paquets en cache au total |  |
| `packageCatalog.clearSearch` | Clear search | Effacer la recherche |  |
| `packageCatalog.hasBlocked` | Has blocked | Contient des versions bloquées |  |
| `packageCatalog.mostDownloaded` | Most Downloaded | Les plus téléchargés |  |
| `packageCatalog.nameAZ` | Name A–Z | Nom A–Z |  |
| `packageCatalog.notYetProxied` | Not Yet Proxied | Pas encore proxifié |  |
| `packageCatalog.pointAToolAt` | Point a tool at this instance | Configurer un outil pour cette instance | **34% longer than English** — check it does not overflow. |
| `packageCatalog.recentlyAccessed` | Recently Accessed | Accédés récemment |  |
| `packageCatalog.registries` | Registries | Registres |  |
| `packageCatalog.searchPackages` | Search packages… | Rechercher des paquets… |  |
| `packageCatalog.searchPackages2` | Search packages | Rechercher des paquets |  |
| `packageCatalog.searchingUpstreamRegistries` | Searching upstream registries… | Recherche dans les registres upstream… | **27% longer than English** — check it does not overflow. |
| `packageCatalog.sortPackages` | Sort packages | Trier les paquets |  |
| `packageDetail.administration` | Administration | Administration |  |
| `packageDetail.noVersionsBody` | Nothing has been pulled through or published to this registry under this name. | Rien n'a transité par cette instance ni été publié sous ce nom dans ce registre. |  |
| `packageDetail.noVersionsTitle` | No versions yet | Aucune version |  |
| `packageDetail.why` | Why? | Pourquoi ? |  |
| `packageDetailPage.accessGate` | Access Gate | Contrôle d'accès |  |
| `packageDetailPage.backToCatalog` | Back to catalog | Retour au catalogue |  |
| `packageDetailPage.betaChannel` | Beta channel: | Canal bêta : |  |
| `packageDetailPage.deprecated` | Deprecated | Déprécié |  |
| `packageDetailPage.downloadCyclonedx14` | Download CycloneDX 1.4 | Télécharger CycloneDX 1.4 |  |
| `packageDetailPage.downloadSpdx23` | Download SPDX 2.3 | Télécharger SPDX 2.3 |  |
| `packageDetailPage.downloadViaProxy` | Download {version} via proxy | Télécharger {version} via le proxy |  |
| `packageDetailPage.fixedIn` | Fixed in: | Corrigé dans : |  |
| `packageDetailPage.knownVersions` | {count} known version \| {count} known versions | {count} version connue \| {count} versions connues |  |
| `packageDetailPage.lastAccessed` | Last Accessed | Dernier accès |  |
| `packageDetailPage.loading` | Loading… | Chargement… |  |
| `packageDetailPage.local` | Local | Local |  |
| `packageDetailPage.memberPreReleaseVersionsVisible` | Member — pre-release versions visible | Membre — versions pre-release visibles |  |
| `packageDetailPage.noSbom` | No SBOM | Aucun SBOM |  |
| `packageDetailPage.noVersionsYet` | No versions yet | Aucune version |  |
| `packageDetailPage.nonMember` | Non-member | Non-membre |  |
| `packageDetailPage.nothingHasBeenPulledThrough` | Nothing has been pulled through or published to this registry under this name. | Rien n'a été récupéré ni publié dans ce registre sous ce nom. |  |
| `packageDetailPage.registryAccess` | Registry access: | Accès au registre : |  |
| `packageDetailPage.supplyChainReportOn` | Supply-chain report on socket.dev | Rapport supply-chain sur socket.dev |  |
| `packageDetailPage.why` | Why? | Pourquoi ? |  |
| `packageEventsTable.noEventsRecordedYet` | No events recorded yet. | Aucun événement enregistré. |  |
| `packageEventsTable.recentAccessEvents` | Recent access events | Événements d'accès récents | **30% longer than English** — check it does not overflow. |
| `packageVersionsTable.available` | Available | Disponible |  |
| `packageVersionsTable.blockSelected` | Block selected | Bloquer la sélection |  |
| `packageVersionsTable.fixedIn` |  (fixed in {version}) |  (corrigé dans {version}) |  |
| `packageVersionsTable.lastAccessed` | Last accessed | Dernier accès |  |
| `packageVersionsTable.lastPulledBy` | Last pulled by | Dernier pull par |  |
| `packageVersionsTable.noVersionsTrackedYet` | No versions tracked yet. | Aucune version suivie. |  |
| `packageVersionsTable.notCached` | Not cached | Non mis en cache |  |
| `packageVersionsTable.purgeCache` | Purge cache | Purger le cache |  |
| `packageVersionsTable.selectAllVersions` | Select all versions | Sélectionner toutes les versions |  |
| `packageVersionsTable.selectVersion` | Select version {version} | Sélectionner la version {version} | **38% longer than English** — check it does not overflow. |
| `packageVersionsTable.supplyChainReportOn` | Supply-chain report on socket.dev | Rapport supply-chain sur socket.dev |  |
| `packageVersionsTable.unblockSelected` | Unblock selected | Débloquer la sélection |  |
| `packageVersionsTable.versionsArtifacts` | Versions & artifacts | Versions et artefacts |  |
| `packageVersionsTable.versionsSelected` | {count} version selected \| {count} versions selected | {count} version sélectionnée \| {count} versions sélectionnées |  |
| `packageVisibility.controlsWhoCanDownload` | Controls who can download this package (all versions share the same setting). | Détermine qui peut télécharger ce paquet (toutes les versions partagent le même réglage). |  |
| `packageVisibility.packageVisibility` | Package visibility | Visibilité du paquet |  |
| `packageVisibility.save` | Save | Enregistrer |  |
| `packageVisibility.saving` | Saving… | Enregistrement… |  |
| `pagination.label` | Pagination | Pagination |  |
| `pagination.page` | Page {page} | Page {page} |  |
| `pagination.pageOf` | Page {page} of {total} | Page {page} sur {total} | **`sur`, not `de`** — French pagination convention is `Page 3 sur 10`. Two whole messages rather than a sentence assembled around a value, because the count is not always known. |
| `pathMapper.httpsPypiOrgProject` | https://pypi.org/project/requests/… or https://github.com/owner/repo/… | https://pypi.org/project/requests/… ou https://github.com/owner/repo/… |  |
| `pathMapper.pasteAnUpstreamUrl` | Upstream URL | URL upstream |  |
| `pathMapper.pasteAnUpstreamUrlOr` | Paste an upstream URL or fill in the fields to get the equivalent proxy path. | Collez une URL upstream ou remplissez les champs pour obtenir le chemin proxy équivalent. |  |
| `pathMapper.registryType` | Registry type | Type de registre |  |
| `pathMapper.urlMapper` | URL Mapper | Mappage d'URL |  |
| `registryPathForm.registryName` | Registry name | Nom du registre |  |
| `registryPathResults.fillInTheFields` | Fill in the fields above to see the proxy paths. | Renseignez les champs ci-dessus pour voir les chemins du proxy. | **31% longer than English** — check it does not overflow. |
| `registryPathResults.needsMoreFields` | needs more fields | champs manquants |  |
| `registryPathResults.proxyPaths` | Proxy paths | Chemins du proxy |  |
| `setup.filterLabel` | Filter tools | Filtrer les outils |  |
| `setup.filterPlaceholder` | Filter tools… | Filtrer les outils… |  |
| `setup.freshBody` | No registries are configured on this instance, or none that your account can reach. Until one exists there is no URL for a tool to point at. | Aucun registre n'est configuré sur cette instance, ou aucun n'est accessible à votre compte. Tant qu'il n'en existe pas, il n'y a pas d'URL vers laquelle pointer un outil. |  |
| `setup.freshTitle` | Nothing to connect to yet | Rien à connecter pour l'instant |  |
| `setup.noToolMatch` | No tool matches “{query}”. | Aucun outil ne correspond à « {query} ». | Uses French quotation marks « » rather than “ ”. Intentional. **54% longer than English** — check it does not overflow. |
| `setupGuide.configurationGuide` | Configuration guide | Guide de configuration |  |
| `setupGuide.configureYourToolsToRoute` | Configure your tools to route package downloads through this proxy. Snippets are pre-filled with this server's address and your configured registries. | Configurez vos outils pour router les téléchargements de paquets via ce proxy. Les extraits sont pré-remplis avec l'adresse de ce serveur et vos registres configurés. |  |
| `setupGuide.filterTools` | Filter tools | Filtrer les outils |  |
| `setupGuide.filterTools2` | Filter tools… | Filtrer les outils… |  |
| `setupGuide.loadingRegistries` | Loading registries… | Chargement des registres… |  |
| `setupGuide.netrcHelp` | Credentials for tools that use HTTP Basic Auth (curl, wget, …). Place in {file} and restrict permissions with {chmod}. | Identifiants pour les outils utilisant l'authentification HTTP Basic (curl, wget, …). À placer dans {file} et à protéger avec {chmod}. |  |
| `setupGuide.noRegistriesAreConfiguredOn` | No registries are configured on this instance, or none that your account can reach. Until one exists there is no URL for a tool to point at. | Aucun registre n'est configuré sur cette instance, ou aucun accessible à votre compte. Tant qu'il n'en existe pas, aucun outil n'a d'URL vers laquelle pointer. |  |
| `setupGuide.nothingToConnectTo` | Nothing to connect to yet | Rien à connecter pour l'instant |  |
| `setupGuide.oidcTokenNote` | Your current token is a short-lived OIDC session token. For long-lived automation, create a {link} and use that as the password. | Votre token actuel est un token de session OIDC de courte durée. Pour de l'automatisation durable, créez un {link} et utilisez-le comme mot de passe. |  |
| `setupGuide.openConfig` | Open config | Ouvrir la configuration |  |
| `setupGuide.personalApiToken` | personal API token | token d'API personnel |  |
| `setupGuide.setupGuide` | Setup Guide | Guide de configuration |  |
| `theme.dark` | Theme: dark | Thème : sombre |  |
| `theme.light` | Theme: light | Thème : clair |  |
| `theme.system` | Theme: follow system | Thème : suivre le système |  |
| `tokensPage.activeTokens` | Active Tokens | Tokens actifs |  |
| `tokensPage.chooseANameRole` | Choose a name, role, and lifetime for your token. | Choisissez un nom, un rôle et une durée de vie pour votre token. | **31% longer than English** — check it does not overflow. |
| `tokensPage.createApiToken` | Create API Token | Créer un token d'API |  |
| `tokensPage.createLongLivedTokens` | Create long-lived tokens for programmatic access. Tokens inherit your current role (or         lower). Maximum lifetime is 90 days. The raw token is shown only once on creation — store it         securely. | Créez des tokens de longue durée pour un accès programmatique. Les tokens héritent de votre rôle actuel (ou d'un rôle inférieur). La durée de vie maximale est de 90 jours. Le token brut n'est affiché qu'une seule fois à la création — conservez-le en lieu sûr. | **26% longer than English** — check it does not overflow. |
| `tokensPage.createToken` | Create token | Créer le token |  |
| `tokensPage.creating` | Creating… | Création… |  |
| `tokensPage.customTokenExpiryIn` | Custom token expiry in days | Expiration personnalisée du token, en jours | **59% longer than English** — check it does not overflow. |
| `tokensPage.dismissAutoClearsIn` | Dismiss (auto-clears in 60 s) | Masquer (disparaît automatiquement dans 60 s) | **55% longer than English** — check it does not overflow. |
| `tokensPage.noActiveTokensCreate` | No active tokens. Create one to get started. | Aucun token actif. Créez-en un pour démarrer. |  |
| `tokensPage.orCustom` | or custom: | ou personnalisée : |  |
| `tokensPage.personalApiTokens` | Personal API Tokens | Tokens d'API personnels |  |
| `tokensPage.revokeTokenNamed` | Revoke token {name} | Révoquer le token {name} |  |
| `tokensPage.selectRole` | Select role | Choisir un rôle |  |
| `tokensPage.tokenCreatedCopyIt` | Token created — copy it now, it won't be shown again. | Token créé — copiez-le maintenant, il ne sera plus affiché. |  |
| `tokensPage.tokensThatHaveNot` | Tokens that have not been revoked and have not yet expired. | Tokens non révoqués et non encore expirés. |  |
| `tools.accessCheck` | Access Check | Vérification d'accès | Translated, because it names a *page*, not an API concept. |
| `tools.description` | Work out why a request was allowed or refused, and what URL a client should use. | Comprendre pourquoi une requête a été autorisée ou refusée, et quelle URL un client doit utiliser. |  |
| `tools.title` | Tools | Outils |  |
| `tools.urlMapper` | URL Mapper | Mappage d'URL | **Decided: `Mappage d'URL`.** Closer to the English and 7 characters shorter, which matters in a tab strip beside `Vérification d'accès`. |
| `userMenu.downloadCli` | Download CLI | Télécharger le CLI |  |
| `userMenu.myNamespace` | My Namespace | Mon namespace |  |
| `userMenu.myProfile` | My Profile | Mon profil |  |
| `userMenu.myTokens` | My Tokens | Mes tokens |  |
| `userMenu.signIn` | Sign in | Se connecter |  |
| `userMenu.signOut` | Sign out | Se déconnecter |  |
| `visibility.internal` | Internal — authenticated users only | Interne — utilisateurs authentifiés uniquement | **31% longer than English** — check it does not overflow. |
| `visibility.public` | Public — anyone can download | Public — téléchargeable par tous | The em-dash gloss structure is kept in all three visibility rows so the select reads as one list. `visibilityShort.*` carries the one-word form used in the table, rather than the long form cut at the dash — French does not put the dash where English does. |
| `visibility.team` | Team — namespace group members only | Équipe — membres du groupe du namespace uniquement | **43% longer than English** — check it does not overflow. |
| `visibilityShort.internal` | Internal | Interne |  |
| `visibilityShort.public` | Public | Public |  |
| `visibilityShort.team` | Team | Équipe |  |

## Not yet translated

`task ui:i18n` reports: **0 untranslated strings across 0 files**, and `task ui:i18n:check`
is pinned there — a new hardcoded string fails the build.

That number has read zero before while most of the console was still English, so
it is worth saying what it now counts. The audit reads four places, because text
hides in all four (RFC 0003 §14):

| Where | Example |
| --- | --- |
| Text nodes | `<th>Registry</th>` |
| Human-facing attributes *and component props* | `title="Dashboard"`, `label="Registries"` |
| Literals inside template expressions | `{{ busy ? 'Loading…' : 'Refresh' }}` |
| Literals in `<script>` | `{ label: "All Packages" }` |

It had been reading only the first, and a case-insensitive identifier rule inside
that one excluded every bare capitalised word — `Registries`, `You`, `Version`.
384 user-visible strings sat behind a green gate; the catalogue went from 418
keys to 646.

Deliberately **not** counted: `:class`/`:style` literals (Tailwind lists, not
prose), and `registryTypes.ts` / `registryPathFields.ts` — the setup-snippet data
RFC 0003 §6.7 keeps as data, whose labels are tool names and config keys that
§4.6 keeps verbatim anyway.
