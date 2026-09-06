---
layout: home

hero:
  image:
    src: /logo.svg
    alt: BatleHub
  name: BatleHub
  text: Your package hub. Proxy, cache, and host.
  tagline: Sit between your build tools and the internet. Cache artifacts, enforce access control, and publish private packages — all from one self-hosted server.
  actions:
    - theme: brand
      text: Get Started
      link: /guide/installation
    - theme: alt
      text: View on Git
      link: https://github.com/batlehub/batlehub

features:
  - icon: ⚡
    title: Cache what you already pull
    details: Every artifact is fetched from upstream once and served from disk or S3 after that. Twenty-one ecosystems, one server, no change to how your build tools are invoked.
    link: /guide/caching
    linkText: How caching works
  - icon: 🔒
    title: Publish what is yours
    details: Private npm packages, Cargo crates, Go modules, Python wheels, NuGet packages and more — on the same server, in the same URL space, published with the tool you already use.
    link: /use/publishing
    linkText: Publishing guide
  - icon: 🛡️
    title: Decide who gets what
    details: Per-registry permissions for anonymous, user and admin roles, groups from OIDC or CI tokens, and gates on what may be pulled at all — by age, by advisory, by licence.
    link: /guide/access-control
    linkText: Access control
---

BatleHub proxies, caches and privately hosts **21 registry types** — from npm and
Cargo to Debian, Terraform and the VS Code marketplace. See the
**[Registries reference](/registries/)** for the feature matrix and a setup page
for each one, or the **[full feature list](/guide/features)** for everything the
server does.
