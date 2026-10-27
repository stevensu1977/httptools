use std::{borrow::Cow, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use url::Url;

use crate::config::{BenchConfig, HttpMethod, RequestConfig, RequestContext, RequestSource};
use crate::error::Result;

/// HTTP client wrapper for benchmark requests
pub struct HttpClient {
    client: Client<HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>, Full<Bytes>>,
    timeout: Duration,
}

impl HttpClient {
    /// Create a new HTTP client with the given timeout and connection pool settings
    pub fn new(timeout: Duration, concurrency: usize) -> Result<Self> {
        let mut connector = hyper_util::client::legacy::connect::HttpConnector::new();
        connector.enforce_http(false);
        connector.set_nodelay(true);
        connector.set_keepalive(Some(Duration::from_secs(60)));

        let https = HttpsConnector::new_with_connector(connector);

        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(concurrency)
            .pool_timer(hyper_util::rt::TokioTimer::new())
            .build(https);

        Ok(HttpClient { client, timeout })
    }

    /// Execute a request, dispatching based on the request source (static or dynamic).
    /// Returns (status_code, bytes_received).
    pub async fn execute_for_worker(
        &self,
        config: &BenchConfig,
        worker_id: usize,
        request_number: usize,
    ) -> Result<(Option<u16>, usize)> {
        match &config.request_source {
            RequestSource::Static(req) => self.execute_request(req).await,
            RequestSource::Dynamic(generator) => {
                let ctx = RequestContext {
                    worker_id,
                    request_number,
                };
                self.execute_request(&generator(ctx)).await
            }
        }
    }

    /// Execute a single HTTP request from RequestConfig.
    /// Returns (status_code, bytes_received).
    pub async fn execute_request(&self, req: &RequestConfig) -> Result<(Option<u16>, usize)> {
        // Bound the entire operation, including redirects and streamed response bodies.
        tokio::time::timeout(self.timeout, self.execute_redirects(req))
            .await
            .map_err(|_| crate::error::Error::Timeout)?
    }

    async fn execute_redirects(&self, original: &RequestConfig) -> Result<(Option<u16>, usize)> {
        let mut req = Cow::Borrowed(original);
        for redirects in 0..=50 {
            let (status, bytes, location) = self.execute_once(&req).await?;
            let Some(location) = location else {
                return Ok((Some(status), bytes));
            };
            if redirects == 50 {
                return Err(crate::error::Error::InvalidConfig(
                    "redirect limit (50) exceeded".into(),
                ));
            }
            let previous =
                Url::parse(&req.url).map_err(|e| crate::error::Error::InvalidUrl(e.to_string()))?;
            let next = previous
                .join(&location)
                .map_err(|e| crate::error::Error::InvalidUrl(e.to_string()))?;
            if !matches!(next.scheme(), "http" | "https")
                || !next.username().is_empty()
                || next.password().is_some()
            {
                return Err(crate::error::Error::InvalidUrl(
                    "redirect must use HTTP(S) without embedded credentials".into(),
                ));
            }
            let req = req.to_mut();
            if previous.origin() != next.origin() {
                req.headers.retain(|name, _| {
                    !["authorization", "proxy-authorization", "cookie", "host"]
                        .iter()
                        .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
                });
            }
            if (matches!(status, 301 | 302) && req.method == HttpMethod::Post)
                || (status == 303 && req.method != HttpMethod::Head)
            {
                req.method = HttpMethod::Get;
                req.body = None;
                req.headers.retain(|name, _| {
                    !["content-type", "content-length", "transfer-encoding"]
                        .iter()
                        .any(|header| name.eq_ignore_ascii_case(header))
                });
            }
            let mut next = next;
            next.set_fragment(None);
            req.url = next.to_string();
        }
        unreachable!("redirect loop always returns at its limit")
    }

    async fn execute_once(&self, req: &RequestConfig) -> Result<(u16, usize, Option<String>)> {
        let method = match req.method {
            HttpMethod::Get => hyper::Method::GET,
            HttpMethod::Post => hyper::Method::POST,
            HttpMethod::Put => hyper::Method::PUT,
            HttpMethod::Delete => hyper::Method::DELETE,
            HttpMethod::Patch => hyper::Method::PATCH,
            HttpMethod::Head => hyper::Method::HEAD,
            HttpMethod::Options => hyper::Method::OPTIONS,
        };

        let uri: hyper::Uri = req.url.parse().map_err(|e: hyper::http::uri::InvalidUri| {
            crate::error::Error::InvalidUrl(e.to_string())
        })?;

        let body = match &req.body {
            Some(b) => Full::new(b.clone()),
            None => Full::new(Bytes::new()),
        };

        let mut builder = Request::builder().method(method).uri(uri);

        for (key, value) in &req.headers {
            builder = builder.header(key.as_str(), value.as_str());
        }

        let request = builder
            .body(body)
            .map_err(|e| crate::error::Error::Http(e.into()))?;

        let response = self
            .client
            .request(request)
            .await
            .map_err(|e| crate::error::Error::Http(e.into()))?;

        let status = response.status().as_u16();
        let location = if matches!(status, 301 | 302 | 303 | 307 | 308) {
            response
                .headers()
                .get(hyper::header::LOCATION)
                .map(|v| v.to_str().map(str::to_owned))
                .transpose()
                .map_err(|e| crate::error::Error::Http(e.into()))?
        } else {
            None
        };

        // Drain in chunks: bounded memory, connection reuse, and body errors count as failures.
        let mut bytes = 0;
        let mut body = response.into_body();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| crate::error::Error::Http(e.into()))?;
            if let Some(data) = frame.data_ref() {
                bytes += data.len();
            }
        }

        Ok((status, bytes, location))
    }
}
