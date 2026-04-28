use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use clap::Parser;
use serde::Serialize;
use toq_rs::{
    default_values_for_type_tags, get_open_tcp_port, get_open_udp_port,
    watch_oscquery_services_with_daemon, DiscoveryEvent, OSCAccess, OSCQueryNode, OSCQueryService,
    OscValue, ServiceDaemon,
};

#[derive(Parser)]
#[command(name = "toq-rs", about = "OSCQuery sidecar — mDNS + HTTP for OSC apps")]
struct Cli {
    /// Service name for mDNS advertisement
    #[arg(long)]
    name: String,

    /// UDP port the parent app listens on for OSC (auto-assigned if omitted)
    #[arg(long)]
    osc_port: Option<u16>,

    /// TCP port for the OSCQuery HTTP server (auto-assigned if omitted)
    #[arg(long)]
    http_port: Option<u16>,

    /// IP address to advertise for OSC
    #[arg(long, default_value = "127.0.0.1")]
    osc_ip: String,

    /// Endpoint specs: /path:type_tags:access (repeatable)
    #[arg(long = "endpoint")]
    endpoints: Vec<String>,

    /// File containing endpoint specs, one per line
    #[arg(long)]
    endpoints_file: Option<PathBuf>,
}

#[derive(Serialize)]
#[serde(tag = "event")]
enum Event {
    #[serde(rename = "ready")]
    Ready { http_port: u16, osc_port: u16 },
    #[serde(rename = "service_discovered")]
    ServiceDiscovered {
        name: String,
        osc_ip: String,
        osc_port: u16,
    },
    #[serde(rename = "service_lost")]
    ServiceLost { name: String },
    #[serde(rename = "error")]
    Error { message: String },
}

fn emit(event: &Event) {
    let mut stdout = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut stdout, event);
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
}

fn parse_endpoint(spec: &str) -> Result<(String, String, OSCAccess), String> {
    let parts: Vec<&str> = spec.splitn(3, ':').collect();
    if parts.len() != 3 {
        return Err(format!(
            "Invalid endpoint format '{spec}': expected /path:type_tags:access"
        ));
    }
    let path = parts[0].to_owned();
    if !path.starts_with('/') {
        return Err(format!("Endpoint path must start with '/': '{path}'"));
    }
    let type_tags = parts[1].to_owned();
    let access = match parts[2] {
        "r" => OSCAccess::ReadOnly,
        "w" => OSCAccess::WriteOnly,
        "rw" => OSCAccess::ReadWrite,
        "n" => OSCAccess::NoValue,
        other => return Err(format!("Unknown access mode '{other}': expected r/w/rw/n")),
    };
    Ok((path, type_tags, access))
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();

    let http_port = cli.http_port.unwrap_or_else(get_open_tcp_port);
    let osc_port = cli.osc_port.unwrap_or_else(get_open_udp_port);

    let mut endpoint_specs = cli.endpoints.clone();
    if let Some(ref path) = cli.endpoints_file {
        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                emit(&Event::Error {
                    message: format!("Failed to read endpoints file '{}': {e}", path.display()),
                });
                std::process::exit(1);
            }
        };
        for line in contents.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                endpoint_specs.push(line.to_owned());
            }
        }
    }

    let mut parsed_endpoints: Vec<(String, Vec<OscValue>, OSCAccess)> = Vec::new();
    for spec in &endpoint_specs {
        let (path, type_tags, access) = match parse_endpoint(spec) {
            Ok(v) => v,
            Err(e) => {
                emit(&Event::Error { message: e });
                std::process::exit(1);
            }
        };
        let values = match default_values_for_type_tags(&type_tags) {
            Ok(v) => v,
            Err(e) => {
                emit(&Event::Error {
                    message: format!("Bad type tags in endpoint '{spec}': {e}"),
                });
                std::process::exit(1);
            }
        };
        parsed_endpoints.push((path, values, access));
    }

    let mdns = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            emit(&Event::Error {
                message: format!("Failed to create mDNS daemon: {e}"),
            });
            std::process::exit(1);
        }
    };

    let svc = match OSCQueryService::with_daemon(
        &cli.name,
        http_port,
        osc_port,
        &cli.osc_ip,
        mdns.clone(),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            emit(&Event::Error { message: e });
            std::process::exit(1);
        }
    };

    for (path, values, access) in parsed_endpoints {
        let mut node = OSCQueryNode::new(&path).with_access(access);
        if !values.is_empty() {
            node = node.with_value(values);
        }
        svc.add_node(node).await;
    }

    emit(&Event::Ready {
        http_port,
        osc_port,
    });

    let mut discovery_rx = match watch_oscquery_services_with_daemon(mdns) {
        Ok(rx) => rx,
        Err(e) => {
            emit(&Event::Error {
                message: format!("Failed to start discovery: {e}"),
            });
            std::process::exit(1);
        }
    };

    let mut known_services: HashMap<String, String> = HashMap::new();

    loop {
        tokio::select! {
            Some(event) = discovery_rx.recv() => {
                match event {
                    DiscoveryEvent::ServiceFound { fullname, info } => {
                        // Filter out our own service
                        if fullname.starts_with(&cli.name) {
                            continue;
                        }
                        let name = info.get_fullname().to_owned();
                        let addrs = info.get_addresses();
                        let ip = addrs
                            .iter()
                            .next()
                            .map(|a| a.to_string())
                            .unwrap_or_else(|| "127.0.0.1".to_owned());
                        let port = info.get_port();
                        known_services.insert(fullname, name.clone());
                        emit(&Event::ServiceDiscovered {
                            name,
                            osc_ip: ip,
                            osc_port: port,
                        });
                    }
                    DiscoveryEvent::ServiceLost { fullname } => {
                        if let Some(name) = known_services.remove(&fullname) {
                            emit(&Event::ServiceLost { name });
                        }
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }

    drop(svc);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_endpoint_valid() {
        let (path, tags, access) = parse_endpoint("/avatar/parameters/X:f:rw").unwrap();
        assert_eq!(path, "/avatar/parameters/X");
        assert_eq!(tags, "f");
        assert_eq!(access, OSCAccess::ReadWrite);
    }

    #[test]
    fn test_parse_endpoint_multi_type() {
        let (path, tags, access) = parse_endpoint("/test:ff:r").unwrap();
        assert_eq!(path, "/test");
        assert_eq!(tags, "ff");
        assert_eq!(access, OSCAccess::ReadOnly);
    }

    #[test]
    fn test_parse_endpoint_all_access_modes() {
        assert_eq!(parse_endpoint("/p:i:r").unwrap().2, OSCAccess::ReadOnly);
        assert_eq!(parse_endpoint("/p:i:w").unwrap().2, OSCAccess::WriteOnly);
        assert_eq!(parse_endpoint("/p:i:rw").unwrap().2, OSCAccess::ReadWrite);
        assert_eq!(parse_endpoint("/p:i:n").unwrap().2, OSCAccess::NoValue);
    }

    #[test]
    fn test_parse_endpoint_missing_parts() {
        assert!(parse_endpoint("/test:f").is_err());
        assert!(parse_endpoint("/test").is_err());
    }

    #[test]
    fn test_parse_endpoint_no_leading_slash() {
        assert!(parse_endpoint("test:f:rw").is_err());
    }

    #[test]
    fn test_parse_endpoint_bad_access() {
        assert!(parse_endpoint("/test:f:x").is_err());
    }
}
