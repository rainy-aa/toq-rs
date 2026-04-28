use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

use crate::node::{OSCHostInfo, OSCQueryNode};

fn shared_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Events emitted by the mDNS discovery watcher.
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    ServiceFound {
        fullname: String,
        info: Box<ServiceInfo>,
    },
    ServiceLost {
        fullname: String,
    },
}

/// Start an event-driven mDNS watcher for `_oscjson._tcp.local.` services,
/// returning a channel that yields `DiscoveryEvent`s.
pub fn watch_oscquery_services() -> Result<mpsc::Receiver<DiscoveryEvent>, String> {
    let mdns = ServiceDaemon::new().map_err(|e| format!("Failed to create mDNS daemon: {e}"))?;
    watch_oscquery_services_with_daemon(mdns)
}

/// Like `watch_oscquery_services` but reuses a caller-provided `ServiceDaemon`
/// to avoid duplicate daemon threads.
pub fn watch_oscquery_services_with_daemon(
    mdns: ServiceDaemon,
) -> Result<mpsc::Receiver<DiscoveryEvent>, String> {
    let receiver = mdns
        .browse("_oscjson._tcp.local.")
        .map_err(|e| format!("Failed to browse OSCQuery services: {e}"))?;

    let (tx, rx) = mpsc::channel(64);

    std::thread::Builder::new()
        .name("toq-mdns-watch".into())
        .stack_size(512 * 1024)
        .spawn(move || {
            let _mdns = mdns;
            while let Ok(event) = receiver.recv() {
                let discovery = match event {
                    ServiceEvent::ServiceResolved(info) => Some(DiscoveryEvent::ServiceFound {
                        fullname: info.get_fullname().to_owned(),
                        info: Box::new(info),
                    }),
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        Some(DiscoveryEvent::ServiceLost { fullname })
                    }
                    _ => None,
                };
                if let Some(evt) = discovery
                    && tx.blocking_send(evt).is_err()
                {
                    break; // receiver dropped
                }
            }
        })
        .map_err(|e| format!("Failed to spawn watcher thread: {e}"))?;

    Ok(rx)
}

/// Browses for OSCQuery and OSC services on the local network via mDNS.
pub struct OSCQueryBrowser {
    _mdns: ServiceDaemon,
    osc_services: Arc<Mutex<HashMap<String, ServiceInfo>>>,
    oscjson_services: Arc<Mutex<HashMap<String, ServiceInfo>>>,
}

impl OSCQueryBrowser {
    pub fn new() -> Self {
        Self::with_daemon(ServiceDaemon::new().expect("Failed to create mDNS daemon"))
    }

    /// Construct a browser reusing a caller-provided `ServiceDaemon`.
    pub fn with_daemon(mdns: ServiceDaemon) -> Self {
        let osc_services: Arc<Mutex<HashMap<String, ServiceInfo>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let oscjson_services: Arc<Mutex<HashMap<String, ServiceInfo>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let osc_receiver = mdns.browse("_osc._udp.local.").expect("Failed to browse OSC");
        let osc_svcs = osc_services.clone();
        std::thread::Builder::new()
            .name("toq-mdns-osc".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                while let Ok(event) = osc_receiver.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            osc_svcs
                                .lock()
                                .unwrap()
                                .insert(info.get_fullname().to_owned(), info);
                        }
                        ServiceEvent::ServiceRemoved(_, name) => {
                            osc_svcs.lock().unwrap().remove(&name);
                        }
                        _ => {}
                    }
                }
            })
            .expect("Failed to spawn OSC browser thread");

        let oscjson_receiver = mdns
            .browse("_oscjson._tcp.local.")
            .expect("Failed to browse OSCQuery");
        let oscjson_svcs = oscjson_services.clone();
        std::thread::Builder::new()
            .name("toq-mdns-oscjson".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                while let Ok(event) = oscjson_receiver.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            oscjson_svcs
                                .lock()
                                .unwrap()
                                .insert(info.get_fullname().to_owned(), info);
                        }
                        ServiceEvent::ServiceRemoved(_, name) => {
                            oscjson_svcs.lock().unwrap().remove(&name);
                        }
                        _ => {}
                    }
                }
            })
            .expect("Failed to spawn OSCQuery browser thread");

        Self {
            _mdns: mdns,
            osc_services,
            oscjson_services,
        }
    }

    /// Returns all discovered OSC (UDP) services.
    pub fn get_discovered_osc(&self) -> Vec<ServiceInfo> {
        self.osc_services
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Returns all discovered OSCQuery (TCP/HTTP) services.
    pub fn get_discovered_oscquery(&self) -> Vec<ServiceInfo> {
        self.oscjson_services
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Find an OSCQuery service whose host name contains the given string.
    pub async fn find_service_by_name(&self, name: &str) -> Option<ServiceInfo> {
        for svc in self.get_discovered_oscquery() {
            let client = OSCQueryClient::new(&svc);
            if let Some(hi) = client.get_host_info().await
                && hi.name.contains(name)
            {
                return Some(svc);
            }
        }
        None
    }

    /// Find all services that have a node at the given OSC address.
    pub async fn find_nodes_by_endpoint_address(
        &self,
        address: &str,
    ) -> Vec<(ServiceInfo, OSCHostInfo, OSCQueryNode)> {
        let mut results = Vec::new();
        for svc in self.get_discovered_oscquery() {
            let client = OSCQueryClient::new(&svc);
            let hi = match client.get_host_info().await {
                Some(hi) => hi,
                None => continue,
            };
            if let Some(node) = client.query_node(address).await {
                results.push((svc, hi, node));
            }
        }
        results
    }
}

impl Default for OSCQueryBrowser {
    fn default() -> Self {
        Self::new()
    }
}

/// An HTTP client for querying a specific OSCQuery service.
pub struct OSCQueryClient {
    base_url: String,
}

impl OSCQueryClient {
    /// Create a client from a discovered mDNS ServiceInfo.
    pub fn new(service_info: &ServiceInfo) -> Self {
        let addresses = service_info.get_addresses();
        let ip = addresses
            .iter()
            .next()
            .expect("No addresses for service");
        let port = service_info.get_port();
        Self {
            base_url: format!("http://{ip}:{port}"),
        }
    }

    /// Create a client from an explicit address and port.
    pub fn from_addr(ip: &str, port: u16) -> Self {
        Self {
            base_url: format!("http://{ip}:{port}"),
        }
    }

    /// Query a node at the given OSC path.
    pub async fn query_node(&self, path: &str) -> Option<OSCQueryNode> {
        let url = format!("{}{}", self.base_url, path);
        let resp = match shared_http_client().get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error querying node: {e}");
                return None;
            }
        };

        if !resp.status().is_success() {
            return None;
        }

        resp.json().await.ok()
    }

    /// Get the HOST_INFO for this service.
    pub async fn get_host_info(&self) -> Option<OSCHostInfo> {
        let url = format!("{}/HOST_INFO", self.base_url);
        let resp = shared_http_client().get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().await.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_parses_node_json() {
        let json = r#"{
            "FULL_PATH": "/avatar/parameters/VRCFaceBlendH",
            "TYPE": "f",
            "ACCESS": 1,
            "VALUE": [0.5],
            "DESCRIPTION": "A face blend parameter"
        }"#;

        let node: OSCQueryNode = serde_json::from_str(json).unwrap();
        assert_eq!(node.full_path.as_deref(), Some("/avatar/parameters/VRCFaceBlendH"));
        assert_eq!(node.osc_type.as_deref(), Some("f"));
        assert_eq!(
            node.value,
            Some(vec![crate::node::OscValue::Float(0.5)])
        );
    }

    #[test]
    fn test_client_parses_host_info_json() {
        let json = r#"{
            "NAME": "VRChat",
            "OSC_IP": "127.0.0.1",
            "OSC_PORT": 9000,
            "OSC_TRANSPORT": "UDP",
            "EXTENSIONS": {"ACCESS": true, "VALUE": true}
        }"#;

        let hi: OSCHostInfo = serde_json::from_str(json).unwrap();
        assert_eq!(hi.name, "VRChat");
        assert_eq!(hi.osc_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(hi.osc_port, Some(9000));
        assert_eq!(hi.osc_transport.as_deref(), Some("UDP"));
    }

    #[tokio::test]
    async fn test_client_with_service() {
        let port = crate::utility::get_open_tcp_port();
        let svc =
            crate::service::OSCQueryService::new("IntegrationTest", port, 9000, "127.0.0.1")
                .await
                .unwrap();

        svc.add_node(
            OSCQueryNode::new("/test/value")
                .with_access(crate::node::OSCAccess::ReadWrite)
                .with_value(vec![crate::node::OscValue::Int(99)]),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let client = OSCQueryClient::from_addr("127.0.0.1", port);

        let hi = client.get_host_info().await.expect("Should get host info");
        assert_eq!(hi.name, "IntegrationTest");

        let node = client
            .query_node("/test/value")
            .await
            .expect("Should get node");
        assert_eq!(
            node.value,
            Some(vec![crate::node::OscValue::Int(99)])
        );

        assert!(client.query_node("/nonexistent").await.is_none());

        drop(svc);
    }
}
