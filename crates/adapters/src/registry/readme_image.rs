//! The [`ReadmeImageFetcher`] the server wires in (RFC 0007-bis §5.1).
//!
//! One client for every registry rather than one per registry, because an image
//! host is a third party by construction: it is not the configured upstream, so
//! none of a registry's credentials, custom headers or CA certificates apply to
//! it, and a per-registry client would only make it look as though they did.
//!
//! It does honour the operator's **proxy** settings, which is the one upstream
//! option that is about this network rather than about that registry — an
//! instance whose egress goes through a corporate proxy has to reach a badge CDN
//! through it too.

use async_trait::async_trait;

use batlehub_core::error::CoreError;
use batlehub_core::ports::ReadmeImageFetcher;
use batlehub_core::services::readme::image::FetchedImage;

use super::http_client::{apply_upstream_tls, fetch_image, UpstreamHttpOptions};

pub struct HttpReadmeImageFetcher {
    client: reqwest::Client,
}

impl HttpReadmeImageFetcher {
    /// Build the fetcher from the instance's shared upstream options.
    ///
    /// Redirects are **disabled** on the client: `fetch_following_redirects`
    /// follows them itself so it can run the SSRF guard on every hop, and a
    /// client that also followed them would follow each one twice — once
    /// unchecked.
    pub fn new(opts: &UpstreamHttpOptions) -> anyhow::Result<Self> {
        let proxy_only = UpstreamHttpOptions {
            proxy_url: opts.proxy_url.clone(),
            proxy_username: opts.proxy_username.clone(),
            proxy_password: opts.proxy_password.clone(),
            no_proxy: opts.no_proxy.clone(),
            // Deliberately not carried: an image host is not the upstream these
            // were configured for. Sending a registry token to a badge CDN
            // because a package author put its URL in a README is precisely the
            // shape of leak this feature must not introduce.
            ..Default::default()
        };
        let client = apply_upstream_tls(
            super::ssrf::public_only_dns(
                reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()),
                &proxy_only,
            ),
            &proxy_only,
        )?
        .build()?;
        Ok(Self { client })
    }
}

#[async_trait]
impl ReadmeImageFetcher for HttpReadmeImageFetcher {
    async fn fetch(
        &self,
        url: &str,
        allowed_hosts: &[String],
        max_bytes: usize,
    ) -> Result<Option<FetchedImage>, CoreError> {
        fetch_image(&self.client, url, allowed_hosts, max_bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard that matters most, asserted against a server that really is
    /// listening: a loopback URL is refused even though something would answer
    /// it.
    ///
    /// This is also why the endpoint's own behaviour is tested through a fake
    /// rather than through this — every in-process mock server is on `127.0.0.1`,
    /// which is exactly what this refuses.
    #[tokio::test]
    async fn a_loopback_image_url_is_refused_even_though_it_would_answer() {
        let mut upstream = mockito::Server::new_async().await;
        let mock = upstream
            .mock("GET", "/badge.svg")
            .with_status(200)
            .with_header("content-type", "image/svg+xml")
            .with_body("<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
            .create_async()
            .await;

        let fetcher = HttpReadmeImageFetcher::new(&UpstreamHttpOptions::default()).unwrap();
        let err = fetcher
            .fetch(&format!("{}/badge.svg", upstream.url()), &[], 1024)
            .await
            .expect_err("a loopback host must be refused");
        assert!(
            err.to_string().contains("SSRF guard"),
            "unexpected error: {err}"
        );
        // And the request was never made.
        mock.expect(0).assert_async().await;
    }

    /// A scheme that is not `http(s)` is not something to try to fetch. `Ok(None)`
    /// rather than an error: the panel shows the chip, which is what it shows for
    /// every other image it cannot get.
    #[tokio::test]
    async fn a_non_http_url_is_not_fetched_at_all() {
        let fetcher = HttpReadmeImageFetcher::new(&UpstreamHttpOptions::default()).unwrap();
        for url in [
            "data:image/png;base64,AAAA",
            "file:///etc/passwd",
            "ftp://example.com/x.png",
        ] {
            assert_eq!(fetcher.fetch(url, &[], 1024).await.unwrap(), None, "{url}");
        }
    }

    #[tokio::test]
    async fn a_url_that_is_not_a_url_is_an_error_not_a_panic() {
        let fetcher = HttpReadmeImageFetcher::new(&UpstreamHttpOptions::default()).unwrap();
        assert!(fetcher.fetch("not a url at all", &[], 1024).await.is_err());
    }
}
