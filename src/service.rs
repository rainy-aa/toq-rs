use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use tokio::sync::RwLock;

use crate::node::{OSCAccess, OSCHostInfo, OSCQueryNode, OscValue};

#[derive(Clone)]
struct AppState {
    root_node: Arc<RwLock<OSCQueryNode>>,
    host_info_json: Bytes,
}

/// An OSCQuery service that advertises via mDNS and serves an HTTP API.
pub struct OSCQueryService {
    root_node: Arc<RwLock<OSCQueryNode>>,
    host_info: Arc<OSCHostInfo>,
    _mdns: ServiceDaemon,
}

impl OSCQueryService {
    /// Create and start a new OSCQuery service with mDNS registration and HTTP server.
    pub async fn new(
        name: &str,
        http_port: u16,
        osc_port: u16,
        osc_ip: &str,
    ) -> Result<Self, String> {
        let mdns =
            ServiceDaemon::new().map_err(|e| format!("Failed to create mDNS daemon: {e}"))?;
        Self::with_daemon(name, http_port, osc_port, osc_ip, mdns).await
    }

    /// Like `new` but reuses a caller-provided `ServiceDaemon` with the service and watchers.
    pub async fn with_daemon(
        name: &str,
        http_port: u16,
        osc_port: u16,
        osc_ip: &str,
        mdns: ServiceDaemon,
    ) -> Result<Self, String> {
        let root_node = Arc::new(RwLock::new(
            OSCQueryNode::new("/").with_description("root node"),
        ));

        let mut extensions = HashMap::with_capacity(5);
        extensions.insert("ACCESS".to_owned(), serde_json::Value::Bool(true));
        extensions.insert("CLIPMODE".to_owned(), serde_json::Value::Bool(false));
        extensions.insert("RANGE".to_owned(), serde_json::Value::Bool(true));
        extensions.insert("TYPE".to_owned(), serde_json::Value::Bool(true));
        extensions.insert("VALUE".to_owned(), serde_json::Value::Bool(true));

        let host_info = Arc::new(OSCHostInfo {
            name: name.to_owned(),
            osc_ip: Some(osc_ip.to_owned()),
            osc_port: Some(osc_port),
            osc_transport: Some("UDP".to_owned()),
            extensions,
        });

        // Pre-serialize HOST_INFO (immutable) to save per-request JSON encode + allocation.
        let host_info_json = Bytes::from(
            serde_json::to_vec(host_info.as_ref())
                .map_err(|e| format!("Failed to serialize host info: {e}"))?,
        );

        let mut oscquery_props: HashMap<String, String> = HashMap::with_capacity(1);
        oscquery_props.insert("txtvers".to_owned(), "1".to_owned());
        // Collect non-loopback IPv4 only (VRChat and others ignore AAAA records).
        let host_ips = if_addrs::get_if_addrs()
            .map(|addrs| {
                addrs
                    .into_iter()
                    .filter(|a| !a.is_loopback() && matches!(a.ip(), std::net::IpAddr::V4(_)))
                    .map(|a| a.ip().to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|_| String::new());
        // Fall back to loopback if no LAN IPv4 was found; otherwise mdns-sd refuses to register.
        let host_ips = if host_ips.is_empty() {
            "127.0.0.1".to_owned()
        } else {
            host_ips
        };

        let oscquery_info = ServiceInfo::new(
            "_oscjson._tcp.local.",
            name,
            &format!("{name}.local."),
            host_ips.as_str(),
            http_port,
            Some(oscquery_props),
        )
        .map_err(|e| format!("Failed to create OSCQuery service info: {e}"))?;
        mdns.register(oscquery_info)
            .map_err(|e| format!("Failed to register OSCQuery service: {e}"))?;

        let mut osc_props: HashMap<String, String> = HashMap::with_capacity(1);
        osc_props.insert("txtvers".to_owned(), "1".to_owned());
        let osc_info = ServiceInfo::new(
            "_osc._udp.local.",
            name,
            &format!("{name}.local."),
            host_ips.as_str(),
            osc_port,
            Some(osc_props),
        )
        .map_err(|e| format!("Failed to create OSC service info: {e}"))?;
        mdns.register(osc_info)
            .map_err(|e| format!("Failed to register OSC service: {e}"))?;

        let listener = tokio::net::TcpListener::bind(("0.0.0.0", http_port))
            .await
            .map_err(|e| format!("Failed to bind HTTP on port {http_port}: {e}"))?;

        let state = AppState {
            root_node: root_node.clone(),
            host_info_json,
        };

        let app = Router::new().fallback(handle_request).with_state(state);

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("HTTP server error: {e}");
            }
        });

        Ok(Self {
            root_node,
            host_info,
            _mdns: mdns,
        })
    }

    /// Add a node to the service's OSC address tree.
    pub async fn add_node(&self, node: OSCQueryNode) {
        let mut root = self.root_node.write().await;
        root.add_child_node(node);
    }

    /// Convenience method to advertise an OSC endpoint with optional value.
    pub async fn advertise_endpoint(
        &self,
        path: &str,
        value: Option<Vec<OscValue>>,
        access: OSCAccess,
    ) {
        let mut node = OSCQueryNode::new(path).with_access(access);
        if let Some(vals) = value {
            node = node.with_value(vals);
        }
        self.add_node(node).await;
    }

    /// Returns a reference to the host info.
    pub fn host_info(&self) -> &OSCHostInfo {
        &self.host_info
    }
}

async fn handle_request(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let uri = request.uri();
    let path = uri.path();
    let query = uri.query().unwrap_or("");

    if path.contains("HOST_INFO") || query.contains("HOST_INFO") {
        return (
            StatusCode::OK,
            [("content-type", "application/json")],
            state.host_info_json.clone(),
        )
            .into_response();
    }

    let root = state.root_node.read().await;
    match root.find_subnode(path) {
        Some(node) => {
            let body = serde_json::to_vec(node).unwrap_or_default();
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                body,
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            [("content-type", "application/json")],
            "OSC Path not found",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_service_host_info() {
        let port = crate::utility::get_open_tcp_port();
        let svc = OSCQueryService::new("TestService", port, 9000, "127.0.0.1").await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let resp = reqwest::get(format!("http://127.0.0.1:{port}/HOST_INFO"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["NAME"], "TestService");
        assert_eq!(json["OSC_PORT"], 9000);

        drop(svc);
    }

    #[tokio::test]
    async fn test_service_node_lookup() {
        let port = crate::utility::get_open_tcp_port();
        let svc = OSCQueryService::new("TestService", port, 9000, "127.0.0.1").await.unwrap();
        svc.add_node(
            OSCQueryNode::new("/test/node")
                .with_access(OSCAccess::ReadWrite)
                .with_value(vec![OscValue::Int(42)]),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let resp = reqwest::get(format!("http://127.0.0.1:{port}/"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let resp = reqwest::get(format!("http://127.0.0.1:{port}/test/node"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["FULL_PATH"], "/test/node");
        assert_eq!(json["VALUE"][0], 42);

        let resp = reqwest::get(format!("http://127.0.0.1:{port}/nonexistent"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        drop(svc);
    }
}
