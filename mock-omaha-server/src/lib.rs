// Copyright 2020 The Fuchsia Authors
//
// Licensed under a BSD-style license <LICENSE-BSD>, Apache License, Version 2.0
// <LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0>, or the MIT
// license <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your option.
// This file may not be copied, modified, or distributed except according to
// those terms.

use anyhow::Error;
use derive_builder::Builder;
use futures::prelude::*;
use hyper::body::Bytes;
use hyper::server::accept::from_stream;
use hyper::server::Server;
use hyper::service::{make_service_fn, service_fn};
use hyper::{header, Body, Method, Request, Response, StatusCode};
use omaha_client::cup_ecdsa::test_support::{
    make_default_private_key_for_test, make_default_public_key_id_for_test,
};
use omaha_client::cup_ecdsa::PublicKeyId;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
#[cfg(not(target_os = "fuchsia"))]
use tokio::net::{TcpListener, TcpStream};
use url::Url;

#[cfg(not(target_os = "fuchsia"))]
use {
    std::io,
    std::pin::Pin,
    std::task::{Context, Poll},
    tokio::task::JoinHandle,
};

#[cfg(all(not(fasync), not(target_os = "fuchsia")))]
use tokio::sync::Mutex;

#[cfg(fasync)]
use {fuchsia_async as fasync, fuchsia_async::Task, fuchsia_sync::Mutex};

#[cfg(all(fasync, target_os = "fuchsia"))]
use fuchsia_async::net::TcpListener;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
pub enum OmahaResponse {
    NoUpdate,
    Update,
    UrgentUpdate,
    InvalidResponse,
    InvalidURL,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResponseAndMetadata {
    pub response: OmahaResponse,
    pub check_assertion: UpdateCheckAssertion,
    pub version: Option<String>,
    pub cohort_assertion: Option<String>,
    pub codebase: String,
    pub package_name: String,
}

impl Default for ResponseAndMetadata {
    fn default() -> ResponseAndMetadata {
        // This default uses examples from Fuchsia, https://fuchsia.dev/
        ResponseAndMetadata {
            response: OmahaResponse::NoUpdate,
            check_assertion: UpdateCheckAssertion::UpdatesEnabled,
            version: Some("0.1.2.3".to_string()),
            cohort_assertion: None,
            codebase: "fuchsia-pkg://integration.test.fuchsia.com/".to_string(),
            package_name:
                "update?hash=deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
                    .to_string(),
        }
    }
}

// The corresponding private key to lib/omaha-client's PublicKey. For testing
// only, since omaha-client never needs to hold a private key.
pub type PrivateKey = p256::ecdsa::SigningKey;

#[derive(Clone, Debug)]
pub struct PrivateKeyAndId {
    pub id: PublicKeyId,
    pub key: PrivateKey,
}

#[derive(Clone, Debug)]
pub struct PrivateKeys {
    pub latest: PrivateKeyAndId,
    pub historical: Vec<PrivateKeyAndId>,
}

impl PrivateKeys {
    pub fn find(&self, id: PublicKeyId) -> Option<&PrivateKey> {
        if self.latest.id == id {
            return Some(&self.latest.key);
        }
        for pair in &self.historical {
            if pair.id == id {
                return Some(&pair.key);
            }
        }
        None
    }
}

pub fn make_default_private_keys_for_test() -> PrivateKeys {
    PrivateKeys {
        latest: PrivateKeyAndId {
            id: make_default_public_key_id_for_test(),
            key: make_default_private_key_for_test(),
        },
        historical: vec![],
    }
}

pub type ResponseMap = HashMap<String, ResponseAndMetadata>;

#[derive(Copy, Clone, Debug, Deserialize)]
pub enum UpdateCheckAssertion {
    UpdatesEnabled,
    UpdatesDisabled,
}

/// Adapt [tokio::net::TcpStream] to work with hyper.
#[cfg(not(target_os = "fuchsia"))]
#[derive(Debug)]
pub enum ConnectionStream {
    Tcp(TcpStream),
}

#[cfg(not(target_os = "fuchsia"))]
impl tokio::io::AsyncRead for ConnectionStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        match &mut *self {
            ConnectionStream::Tcp(t) => Pin::new(t).poll_read(cx, buf),
        }
    }
}

#[cfg(not(target_os = "fuchsia"))]
impl tokio::io::AsyncWrite for ConnectionStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            ConnectionStream::Tcp(t) => Pin::new(t).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            ConnectionStream::Tcp(t) => Pin::new(t).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            ConnectionStream::Tcp(t) => Pin::new(t).poll_shutdown(cx),
        }
    }
}

#[cfg(not(target_os = "fuchsia"))]
struct TcpListenerStream(TcpListener);

#[cfg(not(target_os = "fuchsia"))]
impl Stream for TcpListenerStream {
    type Item = Result<TcpStream, std::io::Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let listener = &mut this.0;
        match listener.poll_accept(cx) {
            Poll::Ready(value) => Poll::Ready(Some(value.map(|(stream, _)| stream))),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[derive(Clone, Debug, Builder)]
#[builder(pattern = "owned")]
#[builder(derive(Debug))]
pub struct OmahaServer {
    #[builder(default, setter(into))]
    pub responses_by_appid: ResponseMap,
    #[builder(default = "make_default_private_keys_for_test()")]
    pub private_keys: PrivateKeys,
    #[builder(default = "None")]
    pub etag_override: Option<String>,
    #[builder(default)]
    pub require_cup: bool,
}

impl OmahaServer {
    /// Sets the special assertion to make on any future update check requests
    pub fn set_all_update_check_assertions(&mut self, value: UpdateCheckAssertion) {
        for response_and_metadata in self.responses_by_appid.values_mut() {
            response_and_metadata.check_assertion = value;
        }
    }

    /// Sets the special assertion to make on any future cohort in requests
    pub fn set_all_cohort_assertions(&mut self, value: Option<String>) {
        for response_and_metadata in self.responses_by_appid.values_mut() {
            response_and_metadata.cohort_assertion = value.clone();
        }
    }

    /// Start the server detached, returning the address of the server
    pub async fn start_and_detach(
        arc_server: Arc<Mutex<OmahaServer>>,
        addr: Option<SocketAddr>,
    ) -> Result<String, Error> {
        let (addr, _server_task) = OmahaServer::start(arc_server, addr).await?;
        #[cfg(fasync)]
        _server_task.expect("no server task found").detach();

        Ok(addr)
    }

    /// Spawn the server on the current executor, returning the address of the server and
    /// its Task.
    #[cfg(all(fasync, target_os = "fuchsia"))]
    pub async fn start(
        arc_server: Arc<Mutex<OmahaServer>>,
        addr: Option<SocketAddr>,
    ) -> Result<(String, Option<Task<()>>), Error> {
        let addr = if let Some(a) = addr {
            a
        } else {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)
        };

        let (connections, addr) = {
            let listener = TcpListener::bind(&addr)?;
            let local_addr = listener.local_addr()?;
            (
                listener
                    .accept_stream()
                    .map_ok(|(conn, _addr)| fuchsia_hyper::TcpStream { stream: conn }),
                local_addr,
            )
        };

        let make_svc = make_service_fn(move |_socket| {
            let arc_server = Arc::clone(&arc_server);
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    let arc_server = Arc::clone(&arc_server);
                    async move { handle_request(req, &arc_server).await }
                }))
            }
        });

        let server = Server::builder(from_stream(connections))
            .executor(fuchsia_hyper::Executor)
            .serve(make_svc)
            .unwrap_or_else(|e| panic!("error serving omaha server: {e}"));

        let server_task = fasync::Task::spawn(server);
        Ok((format!("http://{addr}/"), Some(server_task)))
    }

    /// Spawn the server on the current executor, returning the address of the server and
    /// its Task.
    #[cfg(all(fasync, not(target_os = "fuchsia")))]
    pub async fn start(
        arc_server: Arc<Mutex<OmahaServer>>,
        addr: Option<SocketAddr>,
    ) -> Result<(String, Option<Task<()>>), Error> {
        let addr = if let Some(a) = addr {
            a
        } else {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)
        };

        let make_svc = make_service_fn(move |_socket| {
            let arc_server = Arc::clone(&arc_server);
            async {
                Ok::<_, Infallible>(service_fn(move |req| {
                    let arc_server = Arc::clone(&arc_server);
                    async move { handle_request(req, &arc_server).await }
                }))
            }
        });

        let listener = TcpListener::bind(&addr)
            .await
            .expect("cannot bind to address");

        let addr = listener.local_addr()?;

        let server = async move {
            Server::builder(from_stream(
                TcpListenerStream(listener).map_ok(ConnectionStream::Tcp),
            ))
            .executor(fuchsia_hyper::Executor)
            .serve(make_svc)
            .await
            .unwrap_or_else(|e| panic!("error serving omaha server: {e}"));
        };

        let server_task = fasync::Task::spawn(server);
        Ok((format!("http://{addr}/"), Some(server_task)))
    }

    /// Spawn the server on the current executor, returning the address of the server and
    /// its JoinHandle.
    #[cfg(feature = "tokio")]
    pub async fn start(
        arc_server: Arc<Mutex<OmahaServer>>,
        addr: Option<SocketAddr>,
    ) -> Result<(String, Option<JoinHandle<()>>), Error> {
        let addr = if let Some(a) = addr {
            a
        } else {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)
        };

        let make_svc = make_service_fn(move |_socket| {
            let arc_server = Arc::clone(&arc_server);
            async {
                Ok::<_, Infallible>(service_fn(move |req| {
                    let arc_server = Arc::clone(&arc_server);
                    async move { handle_request(req, &arc_server).await }
                }))
            }
        });

        let listener = TcpListener::bind(&addr)
            .await
            .expect("cannot bind to address");

        let addr = listener.local_addr()?;

        let server_task = tokio::spawn(async move {
            let connections = TcpListenerStream(listener).map_ok(ConnectionStream::Tcp);
            let _ = Server::builder(from_stream(connections))
                .serve(make_svc)
                .await;
        });
        Ok((format!("http://{addr}/"), Some(server_task)))
    }
}

fn make_etag(
    request_body: &Bytes,
    uri: &str,
    private_keys: &PrivateKeys,
    response_data: &[u8],
) -> Option<String> {
    use p256::ecdsa::signature::Signer;

    if uri == "/" {
        return None;
    }

    let parsed_uri = Url::parse(&format!("https://example.com{uri}")).unwrap();
    let mut query_pairs = parsed_uri.query_pairs();

    let (cup2key_key, cup2key_val) = query_pairs.next().unwrap();
    assert_eq!(cup2key_key, "cup2key");

    let (public_key_id_str, _nonce_str) = cup2key_val.split_once(':').unwrap();
    let public_key_id: PublicKeyId = public_key_id_str.parse().unwrap();
    let private_key: &PrivateKey = match private_keys.find(public_key_id) {
        Some(pk) => Some(pk),
        None => {
            tracing::error!(
                "Could not find public_key_id {:?} in the private_keys map, which only knows about the latest key_id {:?} and the historical key_ids {:?}",
                public_key_id,
                private_keys.latest.id,
                private_keys.historical.iter().map(|pkid| pkid.id).collect::<Vec<_>>(),
                );
            None
        }
    }?;

    let request_hash = Sha256::digest(request_body);
    let response_hash = Sha256::digest(response_data);

    let mut hasher = Sha256::new();
    hasher.update(request_hash);
    hasher.update(response_hash);
    hasher.update(&*cup2key_val);
    let transaction_hash = hasher.finalize();

    Some(format!(
        "{}:{}",
        hex::encode(private_key.sign(&transaction_hash).to_der()),
        hex::encode(request_hash)
    ))
}

pub async fn handle_request(
    req: Request<Body>,
    omaha_server: &Mutex<OmahaServer>,
) -> Result<Response<Body>, Error> {
    tracing::debug!("{:#?}", req);
    if req.uri().path() == "/set_responses_by_appid" {
        return handle_set_responses(req, omaha_server).await;
    }

    handle_omaha_request(req, omaha_server).await
}

pub async fn handle_set_responses(
    req: Request<Body>,
    omaha_server: &Mutex<OmahaServer>,
) -> Result<Response<Body>, Error> {
    assert_eq!(req.method(), Method::POST);

    let req_body = hyper::body::to_bytes(req).await?;
    let req_json: HashMap<String, ResponseAndMetadata> =
        serde_json::from_slice(&req_body).expect("parse json");
    {
        #[cfg(feature = "tokio")]
        let mut omaha_server = omaha_server.lock().await;
        #[cfg(fasync)]
        let mut omaha_server = omaha_server.lock();
        omaha_server.responses_by_appid = req_json;
    }

    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, 0);
    Ok(builder.body(Body::empty()).unwrap())
}

pub async fn handle_omaha_request(
    req: Request<Body>,
    omaha_server: &Mutex<OmahaServer>,
) -> Result<Response<Body>, Error> {
    #[cfg(feature = "tokio")]
    let omaha_server = omaha_server.lock().await.clone();
    #[cfg(fasync)]
    let omaha_server = omaha_server.lock().clone();
    assert_eq!(req.method(), Method::POST);

    if omaha_server.responses_by_appid.is_empty() {
        let builder = Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_LENGTH, 0);
        tracing::error!("Received a request before |responses_by_appid| was set; returning an empty response with status 500.");
        return Ok(builder.body(Body::empty()).unwrap());
    }

    let uri_string = req.uri().to_string();

    let req_body = hyper::body::to_bytes(req).await?;
    let req_json: serde_json::Value = serde_json::from_slice(&req_body).expect("parse json");

    let request = req_json.get("request").unwrap();
    let apps = request.get("app").unwrap().as_array().unwrap();

    // If this request contains updatecheck, make sure the mock has the right number of configured apps.
    match apps
        .iter()
        .filter(|app| app.get("updatecheck").is_some())
        .count()
    {
        0 => {}
        x => assert_eq!(x, omaha_server.responses_by_appid.len()),
    }

    let apps: Vec<serde_json::Value> = apps
        .iter()
        .map(|app| {
            let appid = app.get("appid").unwrap();
            let expected = &omaha_server.responses_by_appid[appid.as_str().unwrap()];

            if let Some(expected_version) = &expected.version {
                let version = app.get("version").unwrap();
                assert_eq!(version, expected_version);
            }

            let app = if let Some(expected_update_check) = app.get("updatecheck") {
                let updatedisabled = expected_update_check
                    .get("updatedisabled")
                    .map(|v| v.as_bool().unwrap())
                    .unwrap_or(false);
                match expected.check_assertion {
                    UpdateCheckAssertion::UpdatesEnabled => {
                        assert!(!updatedisabled);
                    }
                    UpdateCheckAssertion::UpdatesDisabled => {
                        assert!(updatedisabled);
                    }
                }

                if let Some(cohort_assertion) = &expected.cohort_assertion {
                    assert_eq!(
                        app.get("cohort")
                            .expect("expected cohort")
                            .as_str()
                            .expect("cohort is string"),
                        cohort_assertion
                    );
                }

                let updatecheck = match expected.response {
                    OmahaResponse::Update => json!({
                        "status": "ok",
                        "urls": {
                            "url": [
                                {
                                    "codebase": expected.codebase,
                                }
                            ]
                        },
                        "manifest": {
                            "version": "0.1.2.3",
                            "actions": {
                                "action": [
                                    {
                                        "run": &expected.package_name,
                                        "event": "install"
                                    },
                                    {
                                        "event": "postinstall"
                                    }
                                ]
                            },
                            "packages": {
                                "package": [
                                    {
                                        "name": &expected.package_name,
                                        "fp": "2.0.1.2.3",
                                        "required": true
                                    }
                                ]
                            }
                        }
                    }),
                    OmahaResponse::UrgentUpdate => json!({
                        "status": "ok",
                        "urls": {
                            "url": [
                                {
                                    "codebase": expected.codebase,
                                }
                            ]
                        },
                        "manifest": {
                            "version": "0.1.2.3",
                            "actions": {
                                "action": [
                                    {
                                        "run": &expected.package_name,
                                        "event": "install"
                                    },
                                    {
                                        "event": "postinstall"
                                    }
                                ]
                            },
                            "packages": {
                                "package": [
                                    {
                                        "name": &expected.package_name,
                                        "fp": "2.0.1.2.3",
                                        "required": true
                                    }
                                ]
                            }
                        },
                        "_urgent_update": true
                    }),
                    OmahaResponse::NoUpdate => json!({
                        "status": "noupdate",
                    }),
                    OmahaResponse::InvalidResponse => json!({
                        "invalid_status": "invalid",
                    }),
                    OmahaResponse::InvalidURL => json!({
                        "status": "ok",
                        "urls": {
                            "url": [
                                {
                                    "codebase": "http://integration.test.fuchsia.com/"
                                }
                            ]
                        },
                        "manifest": {
                            "version": "0.1.2.3",
                            "actions": {
                                "action": [
                                    {
                                        "run": &expected.package_name,
                                        "event": "install"
                                    },
                                    {
                                        "event": "postinstall"
                                    }
                                ]
                            },
                            "packages": {
                                "package": [
                                    {
                                        "name": &expected.package_name,
                                        "fp": "2.0.1.2.3",
                                        "required": true
                                    }
                                ]
                            }
                        }
                    }),
                };
                json!(
                {
                    "cohorthint": "integration-test",
                    "appid": appid,
                    "cohort": "1:1:",
                    "status": "ok",
                    "cohortname": "integration-test",
                    "updatecheck": updatecheck,
                })
            } else {
                assert!(app.get("event").is_some());
                json!(
                {
                    "cohorthint": "integration-test",
                    "appid": appid,
                    "cohort": "1:1:",
                    "status": "ok",
                    "cohortname": "integration-test",
                })
            };
            app
        })
        .collect();
    let response = json!({
        "response": {
            "server": "prod",
            "protocol": "3.0",
            "daystart": {
                "elapsed_seconds": 48810,
                "elapsed_days": 4775
            },
            "app": apps
        }
    });

    let response_data: Vec<u8> = serde_json::to_vec(&response).unwrap();

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, response_data.len());

    // It is only possible to calculate an induced etag if the incoming request
    // had a valid cup2key query argument.
    let induced_etag: Option<String> = make_etag(
        &req_body,
        &uri_string,
        &omaha_server.private_keys,
        &response_data,
    );

    if omaha_server.require_cup && induced_etag.is_none() {
        panic!(
            "mock-omaha-server was configured to expect CUP, but we received a request without it."
        );
    }

    if let Some(etag) = omaha_server
        .etag_override
        .as_ref()
        .or(induced_etag.as_ref())
    {
        builder = builder.header(header::ETAG, etag);
    }

    Ok(builder.body(Body::from(response_data)).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    #[cfg(fasync)]
    use fuchsia_async as fasync;
    #[cfg(feature = "tokio")]
    use hyper::client::HttpConnector;
    use hyper::Client;

    #[cfg(fasync)]
    async fn new_http_client() -> Client<fuchsia_hyper::HyperConnector> {
        fuchsia_hyper::new_client()
    }

    #[cfg(feature = "tokio")]
    async fn new_http_client() -> Client<HttpConnector> {
        Client::new()
    }

    #[cfg_attr(fasync, fasync::run_singlethreaded(test))]
    #[cfg_attr(feature = "tokio", tokio::test)]
    async fn test_no_validate_version() -> Result<(), Error> {
        // Send a request with no specified version and assert that we don't check.
        // See 0.0.0.1 vs 9.9.9.9 below.
        let server = OmahaServer::start_and_detach(
            Arc::new(Mutex::new(
                OmahaServerBuilder::default()
                    .responses_by_appid([(
                        "integration-test-appid-1".to_string(),
                        ResponseAndMetadata {
                            response: OmahaResponse::NoUpdate,
                            version: None,
                            ..Default::default()
                        },
                    )])
                    .build()
                    .unwrap(),
            )),
            None,
        )
        .await
        .context("starting server")?;

        let client = new_http_client().await;
        let body = json!({
            "request": {
                "app": [
                    {
                        "appid": "integration-test-appid-1",
                        "version": "9.9.9.9",
                        "updatecheck": { "updatedisabled": false }
                    },
                ]
            }
        });
        let request = Request::post(&server)
            .body(Body::from(body.to_string()))
            .unwrap();

        let response = client.request(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response)
            .await
            .context("reading response body")?;
        let obj: serde_json::Value =
            serde_json::from_slice(&body).context("parsing response json")?;

        let response = obj.get("response").unwrap();
        let apps = response.get("app").unwrap().as_array().unwrap();
        assert_eq!(apps.len(), 1);
        let status = apps[0].get("updatecheck").unwrap().get("status").unwrap();
        assert_eq!(status, "noupdate");
        Ok(())
    }

    #[cfg_attr(fasync, fasync::run_singlethreaded(test))]
    #[cfg_attr(feature = "tokio", tokio::test)]
    async fn test_server_replies() -> Result<(), Error> {
        let server_url = OmahaServer::start_and_detach(
            Arc::new(Mutex::new(
                OmahaServerBuilder::default()
                    .responses_by_appid([
                        (
                            "integration-test-appid-1".to_string(),
                            ResponseAndMetadata {
                                response: OmahaResponse::NoUpdate,
                                version: Some("0.0.0.1".to_string()),
                                ..Default::default()
                            },
                        ),
                        (
                            "integration-test-appid-2".to_string(),
                            ResponseAndMetadata {
                                response: OmahaResponse::NoUpdate,
                                version: Some("0.0.0.2".to_string()),
                                ..Default::default()
                            },
                        ),
                    ])
                    .build()
                    .unwrap(),
            )),
            None,
        )
        .await
        .context("starting server")?;

        {
            let client = new_http_client().await;
            let body = json!({
                "request": {
                    "app": [
                        {
                            "appid": "integration-test-appid-1",
                            "version": "0.0.0.1",
                            "updatecheck": { "updatedisabled": false }
                        },
                        {
                            "appid": "integration-test-appid-2",
                            "version": "0.0.0.2",
                            "updatecheck": { "updatedisabled": false }
                        },
                    ]
                }
            });
            let request = Request::post(&server_url)
                .body(Body::from(body.to_string()))
                .unwrap();

            let response = client.request(request).await?;

            assert_eq!(response.status(), StatusCode::OK);
            let body = hyper::body::to_bytes(response)
                .await
                .context("reading response body")?;
            let obj: serde_json::Value =
                serde_json::from_slice(&body).context("parsing response json")?;

            let response = obj.get("response").unwrap();
            let apps = response.get("app").unwrap().as_array().unwrap();
            assert_eq!(apps.len(), 2);
            for app in apps {
                let status = app.get("updatecheck").unwrap().get("status").unwrap();
                assert_eq!(status, "noupdate");
            }
        }

        {
            // change the expected responses; now we only configure one app,
            // 'integration-test-appid-1', which will respond with an update.
            let body = json!({
                "integration-test-appid-1": {
                    "response": "Update",
                    "check_assertion": "UpdatesEnabled",
                    "version": "0.0.0.1",
                    "codebase": "fuchsia-pkg://integration.test.fuchsia.com/",
                    "package_name": "update?hash=deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                }
            });
            let request = Request::post(format!("{server_url}set_responses_by_appid"))
                .body(Body::from(body.to_string()))
                .unwrap();
            let client = new_http_client().await;
            let response = client.request(request).await?;
            assert_eq!(response.status(), StatusCode::OK);
        }

        {
            let body = json!({
                "request": {
                    "app": [
                        {
                            "appid": "integration-test-appid-1",
                            "version": "0.0.0.1",
                            "updatecheck": { "updatedisabled": false }
                        },
                    ]
                }
            });
            let request = Request::post(&server_url)
                .body(Body::from(body.to_string()))
                .unwrap();

            let client = new_http_client().await;
            let response = client.request(request).await?;

            assert_eq!(response.status(), StatusCode::OK);
            let body = hyper::body::to_bytes(response)
                .await
                .context("reading response body")?;
            let obj: serde_json::Value =
                serde_json::from_slice(&body).context("parsing response json")?;

            let response = obj.get("response").unwrap();
            let apps = response.get("app").unwrap().as_array().unwrap();
            assert_eq!(apps.len(), 1);
            for app in apps {
                let status = app.get("updatecheck").unwrap().get("status").unwrap();
                // We configured 'integration-test-appid-1' to respond with an update.
                assert_eq!(status, "ok");
            }
        }

        Ok(())
    }

    #[cfg_attr(fasync, fasync::run_singlethreaded(test))]
    #[cfg_attr(feature = "tokio", tokio::test)]
    async fn test_no_configured_responses() -> Result<(), Error> {
        let server = OmahaServer::start_and_detach(
            Arc::new(Mutex::new(
                OmahaServerBuilder::default()
                    .responses_by_appid([])
                    .build()
                    .unwrap(),
            )),
            None,
        )
        .await
        .context("starting server")?;

        let client = new_http_client().await;
        let body = json!({
            "request": {
                "app": [
                    {
                        "appid": "integration-test-appid-1",
                        "version": "0.1.2.3",
                        "updatecheck": { "updatedisabled": false }
                    },
                ]
            }
        });
        let request = Request::post(&server)
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = client.request(request).await?;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        Ok(())
    }

    #[cfg_attr(fasync, fasync::run_singlethreaded(test))]
    #[cfg_attr(feature = "tokio", tokio::test)]
    async fn test_server_expect_cup_nopanic() -> Result<(), Error> {
        let server_url = OmahaServer::start_and_detach(
            Arc::new(Mutex::new(
                OmahaServerBuilder::default()
                    .responses_by_appid([(
                        "integration-test-appid-1".to_string(),
                        ResponseAndMetadata {
                            response: OmahaResponse::NoUpdate,
                            version: Some("0.0.0.1".to_string()),
                            ..Default::default()
                        },
                    )])
                    .require_cup(true)
                    .build()
                    .unwrap(),
            )),
            None,
        )
        .await
        .context("starting server")?;

        let client = new_http_client().await;
        let body = json!({
            "request": {
                "app": [
                    {
                        "appid": "integration-test-appid-1",
                        "version": "0.0.0.1",
                        "updatecheck": { "updatedisabled": false }
                    },
                ]
            }
        });
        // CUP attached.
        let request = Request::post(format!(
            "{}?cup2key={}:nonce",
            &server_url,
            make_default_public_key_id_for_test()
        ))
        .body(Body::from(body.to_string()))
        .unwrap();

        let response = client.request(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    #[cfg_attr(
        fasync,
        fasync::run_singlethreaded(test),
        should_panic(expected = "configured to expect CUP")
    )]
    #[cfg_attr(
        feature = "tokio",
        tokio::test,
        should_panic(
            expected = "called `Result::unwrap()` on an `Err` value: hyper::Error(IncompleteMessage)"
        )
    )]
    async fn test_server_expect_cup_panic() {
        let server_url = OmahaServer::start_and_detach(
            Arc::new(Mutex::new(
                OmahaServerBuilder::default()
                    .responses_by_appid([(
                        "integration-test-appid-1".to_string(),
                        ResponseAndMetadata {
                            response: OmahaResponse::NoUpdate,
                            version: Some("0.0.0.1".to_string()),
                            ..Default::default()
                        },
                    )])
                    .require_cup(true)
                    .build()
                    .unwrap(),
            )),
            None,
        )
        .await
        .context("starting server")
        .unwrap();

        let client = new_http_client().await;
        let body = json!({
            "request": {
                "app": [
                    {
                        "appid": "integration-test-appid-1",
                        "version": "0.0.0.1",
                        "updatecheck": { "updatedisabled": false }
                    },
                ]
            }
        });
        // no CUP, but we set .require_cup(true) above, so mock-omaha-server will
        // panic. (See should_panic above.)
        let request = Request::post(&server_url)
            .body(Body::from(body.to_string()))
            .unwrap();
        let _response = client.request(request).await.unwrap();
    }
}
