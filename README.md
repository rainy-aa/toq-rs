# toq-rs

Rust [OSCQuery](https://github.com/Vidvox/OSCQueryProposal) library and CLI sidecar. Handles mDNS advertisement, HTTP endpoint serving, and network service discovery so your app can focus on OSC.

Built for use with VRChat but works with any OSCQuery-compatible application.

## CLI Usage

```
toq-rs --name "MyApp" \
       --osc-port 9001 \
       --http-port 8080 \
       --endpoint "/avatar/parameters/MyParam:f:rw" \
       --endpoint "/avatar/parameters/MyBool:T:r"
```

| Flag | Required | Default | Description |
|------|----------|---------|-------------|
| `--name` | yes | — | Service name for mDNS |
| `--osc-port` | no | auto | UDP port your app listens on |
| `--http-port` | no | auto | TCP port for OSCQuery HTTP |
| `--osc-ip` | no | 127.0.0.1 | IP to advertise |
| `--endpoint` | no | — | `/path:type_tags:access` (repeatable) |
| `--endpoints-file` | no | — | Text file with one endpoint per line |

Endpoint format: `/path:type:access` where type is OSC tags (`f`, `i`, `T`, `s`, etc.) and access is `r`/`w`/`rw`/`n`.

Endpoints file example:
```
# comments and blank lines are ignored
/avatar/parameters/MyParam:f:rw
/avatar/parameters/MyBool:T:r
/avatar/change:s:rw
```

## Stdout Protocol

One JSON object per line. Your app reads stdout line-by-line.

```json
{"event":"ready","http_port":8080,"osc_port":9001}
{"event":"service_discovered","name":"VRChat-Client-ABC123._oscjson._tcp.local.","osc_ip":"127.0.0.1","osc_port":9000}
{"event":"service_lost","name":"VRChat-Client-ABC123._oscjson._tcp.local."}
{"event":"error","message":"Failed to bind HTTP on port 8080: address already in use"}
```

Stderr is reserved for diagnostics.

## As a Library

```rust
use toq_rs::{OSCQueryService, OSCQueryNode, OSCAccess, OscValue};

let svc = OSCQueryService::new("MyApp", 8080, 9001, "127.0.0.1").await?;
svc.advertise_endpoint(
    "/avatar/parameters/MyParam",
    Some(vec![OscValue::Float(0.0)]),
    OSCAccess::ReadWrite,
).await;
```

## Building

```
cargo build --release
```

## License

MIT
