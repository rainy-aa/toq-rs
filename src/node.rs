use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// OSC access level for a node's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OSCAccess {
    NoValue = 0,
    ReadOnly = 1,
    WriteOnly = 2,
    ReadWrite = 3,
}

impl OSCAccess {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::NoValue),
            1 => Some(Self::ReadOnly),
            2 => Some(Self::WriteOnly),
            3 => Some(Self::ReadWrite),
            _ => None,
        }
    }
}

/// A typed OSC value.
#[derive(Debug, Clone, PartialEq)]
pub enum OscValue {
    Int(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Bool(bool),
    String(String),
}

impl OscValue {
    /// Returns the OSC type tag character for this value.
    pub fn type_tag(&self) -> char {
        match self {
            OscValue::Int(_) => 'i',
            OscValue::Float(_) => 'f',
            OscValue::Long(_) => 'h',
            OscValue::Double(_) => 'd',
            OscValue::Bool(_) => 'T',
            OscValue::String(_) => 's',
        }
    }

    fn to_json_value(&self) -> serde_json::Value {
        match self {
            OscValue::Int(i) => serde_json::Value::from(*i),
            OscValue::Float(f) => serde_json::Number::from_f64(*f as f64)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            OscValue::Long(l) => serde_json::Value::from(*l),
            OscValue::Double(d) => serde_json::Number::from_f64(*d)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            OscValue::Bool(b) => serde_json::Value::from(*b),
            OscValue::String(s) => serde_json::Value::from(s.as_str()),
        }
    }
}

/// Convert a slice of OscValues to their OSC type tag string.
pub fn osc_type_to_tags(values: &[OscValue]) -> String {
    values.iter().map(|v| v.type_tag()).collect()
}

/// Convert an OSC type tag string into a list of type tag chars,
/// skipping whitespace for compatibility with space-separated formats.
pub fn tags_to_type_chars(tags: &str) -> Vec<char> {
    tags.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Parse a JSON value according to an OSC type tag character.
pub fn parse_value_with_tag(tag: char, json_val: &serde_json::Value) -> Option<OscValue> {
    match tag {
        'i' => json_val.as_i64().map(|v| OscValue::Int(v as i32)),
        'f' | 't' => json_val.as_f64().map(|v| OscValue::Float(v as f32)),
        'h' => json_val.as_i64().map(OscValue::Long),
        'd' => json_val.as_f64().map(OscValue::Double),
        'T' | 'F' => json_val.as_bool().map(OscValue::Bool),
        's' => json_val.as_str().map(|s| OscValue::String(s.to_owned())),
        _ => None,
    }
}

/// Map each type tag character to a zero/default OscValue.
pub fn default_values_for_type_tags(tags: &str) -> Result<Vec<OscValue>, String> {
    tags.chars()
        .map(|c| match c {
            'i' => Ok(OscValue::Int(0)),
            'f' | 't' => Ok(OscValue::Float(0.0)),
            'h' => Ok(OscValue::Long(0)),
            'd' => Ok(OscValue::Double(0.0)),
            'T' | 'F' => Ok(OscValue::Bool(false)),
            's' => Ok(OscValue::String(String::new())),
            other => Err(format!("Unknown OSC type tag: '{other}'")),
        })
        .collect()
}

/// An OSCQuery node in the address tree.
#[derive(Debug, Clone)]
pub struct OSCQueryNode {
    pub full_path: Option<String>,
    pub contents: Option<Vec<OSCQueryNode>>,
    pub osc_type: Option<String>,
    pub access: Option<OSCAccess>,
    pub value: Option<Vec<OscValue>>,
    pub description: Option<String>,
}

impl OSCQueryNode {
    pub fn new(full_path: &str) -> Self {
        Self {
            full_path: Some(full_path.to_owned()),
            contents: None,
            osc_type: None,
            access: None,
            value: None,
            description: None,
        }
    }

    pub fn with_description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_owned());
        self
    }

    pub fn with_access(mut self, access: OSCAccess) -> Self {
        self.access = Some(access);
        self
    }

    pub fn with_value(mut self, value: Vec<OscValue>) -> Self {
        self.osc_type = Some(osc_type_to_tags(&value));
        self.value = Some(value);
        self
    }

    pub fn with_osc_type(mut self, type_tags: &str) -> Self {
        self.osc_type = Some(type_tags.to_owned());
        self
    }

    /// Find a subnode by its full path (recursive DFS).
    pub fn find_subnode(&self, path: &str) -> Option<&OSCQueryNode> {
        if self.full_path.as_deref() == Some(path) {
            return Some(self);
        }
        if let Some(contents) = &self.contents {
            for child in contents {
                if let Some(found) = child.find_subnode(path) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// Find a mutable subnode by its full path.
    pub fn find_subnode_mut(&mut self, path: &str) -> Option<&mut OSCQueryNode> {
        if self.full_path.as_deref() == Some(path) {
            return Some(self);
        }
        if let Some(contents) = &mut self.contents {
            for child in contents {
                if let Some(found) = child.find_subnode_mut(path) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// Add a child node, auto-creating intermediate nodes as needed.
    pub fn add_child_node(&mut self, child: OSCQueryNode) {
        let child_path = match &child.full_path {
            Some(p) => p.clone(),
            None => return,
        };

        if self.full_path.as_deref() == Some(child_path.as_str()) {
            return;
        }

        let parent_path = match child_path.rsplit_once('/') {
            Some(("", _)) => "/".to_owned(),
            Some((parent, _)) => parent.to_owned(),
            None => return,
        };

        if self.find_subnode(&parent_path).is_none() {
            let parent_node = OSCQueryNode::new(&parent_path);
            self.add_child_node(parent_node);
        }

        let parent = self.find_subnode_mut(&parent_path).unwrap();
        if parent.contents.is_none() {
            parent.contents = Some(Vec::new());
        }
        parent.contents.as_mut().unwrap().push(child);
    }
}

impl fmt::Display for OSCQueryNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<OSCQueryNode @ {} (D: {:?} T:{:?} V:{:?})>",
            self.full_path.as_deref().unwrap_or("None"),
            self.description,
            self.osc_type,
            self.value
        )
    }
}

impl Serialize for OSCQueryNode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut count = 0;
        if self.full_path.is_some() {
            count += 1;
        }
        if self.osc_type.is_some() {
            count += 1;
        }
        if self.access.is_some() {
            count += 1;
        }
        if self.value.is_some() {
            count += 1;
        }
        if self.description.is_some() {
            count += 1;
        }
        if self.contents.is_some() {
            count += 1;
        }

        let mut map = serializer.serialize_map(Some(count))?;

        if let Some(ref path) = self.full_path {
            map.serialize_entry("FULL_PATH", path)?;
        }
        if let Some(ref osc_type) = self.osc_type {
            map.serialize_entry("TYPE", osc_type)?;
        }
        if let Some(access) = self.access {
            map.serialize_entry("ACCESS", &(access as u8))?;
        }
        if let Some(ref value) = self.value {
            let json_values: Vec<serde_json::Value> =
                value.iter().map(OscValue::to_json_value).collect();
            map.serialize_entry("VALUE", &json_values)?;
        }
        if let Some(ref desc) = self.description {
            map.serialize_entry("DESCRIPTION", desc)?;
        }
        if let Some(ref contents) = self.contents {
            let mut contents_map = serde_json::Map::new();
            for node in contents {
                if let Some(ref path) = node.full_path {
                    let key = path.split('/').next_back().unwrap_or(path);
                    contents_map.insert(
                        key.to_owned(),
                        serde_json::to_value(node).map_err(serde::ser::Error::custom)?,
                    );
                }
            }
            map.serialize_entry("CONTENTS", &contents_map)?;
        }

        map.end()
    }
}

impl<'de> Deserialize<'de> for OSCQueryNode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NodeVisitor;

        impl<'de> Visitor<'de> for NodeVisitor {
            type Value = OSCQueryNode;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an OSCQuery node object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut full_path: Option<String> = None;
                let mut osc_type: Option<String> = None;
                let mut access: Option<OSCAccess> = None;
                let mut value_json: Option<Vec<serde_json::Value>> = None;
                let mut description: Option<String> = None;
                let mut contents: Option<Vec<OSCQueryNode>> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "FULL_PATH" => full_path = Some(map.next_value()?),
                        "TYPE" => osc_type = Some(map.next_value()?),
                        "ACCESS" => {
                            let v: u8 = map.next_value()?;
                            access = OSCAccess::from_u8(v);
                        }
                        "VALUE" => value_json = Some(map.next_value()?),
                        "DESCRIPTION" => description = Some(map.next_value()?),
                        "CONTENTS" => {
                            let contents_map: serde_json::Map<String, serde_json::Value> =
                                map.next_value()?;
                            let mut nodes = Vec::new();
                            for (_key, val) in contents_map {
                                let node: OSCQueryNode =
                                    serde_json::from_value(val).map_err(de::Error::custom)?;
                                nodes.push(node);
                            }
                            contents = Some(nodes);
                        }
                        _ => {
                            let _: serde_json::Value = map.next_value()?;
                        }
                    }
                }

                let value = match (&value_json, &osc_type) {
                    (Some(vals), Some(type_str)) => {
                        let tags = tags_to_type_chars(type_str);
                        let mut parsed = Vec::new();
                        for (idx, v) in vals.iter().enumerate() {
                            if v.is_object()
                                && v.as_object().is_some_and(serde_json::Map::is_empty)
                            {
                                parsed.clear();
                                break;
                            }
                            if let Some(tag) = tags.get(idx) {
                                if let Some(parsed_val) = parse_value_with_tag(*tag, v) {
                                    parsed.push(parsed_val);
                                }
                            }
                        }
                        if parsed.is_empty() {
                            None
                        } else {
                            Some(parsed)
                        }
                    }
                    (Some(vals), None) => {
                        let mut parsed = Vec::new();
                        for v in vals {
                            if let Some(b) = v.as_bool() {
                                parsed.push(OscValue::Bool(b));
                            } else if let Some(i) = v.as_i64() {
                                parsed.push(OscValue::Int(i as i32));
                            } else if let Some(f) = v.as_f64() {
                                parsed.push(OscValue::Float(f as f32));
                            } else if let Some(s) = v.as_str() {
                                parsed.push(OscValue::String(s.to_owned()));
                            }
                        }
                        if parsed.is_empty() {
                            None
                        } else {
                            Some(parsed)
                        }
                    }
                    _ => None,
                };

                Ok(OSCQueryNode {
                    full_path,
                    contents,
                    osc_type,
                    access,
                    value,
                    description,
                })
            }
        }

        deserializer.deserialize_map(NodeVisitor)
    }
}

/// OSCQuery host information.
#[derive(Debug, Clone)]
pub struct OSCHostInfo {
    pub name: String,
    pub osc_ip: Option<String>,
    pub osc_port: Option<u16>,
    pub osc_transport: Option<String>,
    pub extensions: HashMap<String, serde_json::Value>,
}

impl Serialize for OSCHostInfo {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut count = 2; // NAME + EXTENSIONS always present
        if self.osc_ip.is_some() {
            count += 1;
        }
        if self.osc_port.is_some() {
            count += 1;
        }
        if self.osc_transport.is_some() {
            count += 1;
        }

        let mut map = serializer.serialize_map(Some(count))?;
        map.serialize_entry("NAME", &self.name)?;
        if let Some(ref ip) = self.osc_ip {
            map.serialize_entry("OSC_IP", ip)?;
        }
        if let Some(port) = self.osc_port {
            map.serialize_entry("OSC_PORT", &port)?;
        }
        if let Some(ref transport) = self.osc_transport {
            map.serialize_entry("OSC_TRANSPORT", transport)?;
        }
        map.serialize_entry("EXTENSIONS", &self.extensions)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for OSCHostInfo {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HostInfoVisitor;

        impl<'de> Visitor<'de> for HostInfoVisitor {
            type Value = OSCHostInfo;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an OSCQuery host info object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut name: Option<String> = None;
                let mut osc_ip: Option<String> = None;
                let mut osc_port: Option<u16> = None;
                let mut osc_transport: Option<String> = None;
                let mut extensions: Option<HashMap<String, serde_json::Value>> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "NAME" => name = Some(map.next_value()?),
                        "OSC_IP" => osc_ip = Some(map.next_value()?),
                        "OSC_PORT" => osc_port = Some(map.next_value()?),
                        "OSC_TRANSPORT" => osc_transport = Some(map.next_value()?),
                        "EXTENSIONS" => extensions = Some(map.next_value()?),
                        _ => {
                            let _: serde_json::Value = map.next_value()?;
                        }
                    }
                }

                let name = name.ok_or_else(|| de::Error::missing_field("NAME"))?;

                Ok(OSCHostInfo {
                    name,
                    osc_ip,
                    osc_port,
                    osc_transport,
                    extensions: extensions.unwrap_or_default(),
                })
            }
        }

        deserializer.deserialize_map(HostInfoVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_child_and_find_subnode() {
        let mut root = OSCQueryNode::new("/").with_description("root node");
        root.add_child_node(OSCQueryNode::new("/test/node/one"));
        root.add_child_node(OSCQueryNode::new("/test/node/two"));
        root.add_child_node(OSCQueryNode::new("/test/othernode/one"));

        assert!(root.find_subnode("/").is_some());
        assert!(root.find_subnode("/test").is_some());
        assert!(root.find_subnode("/test/node").is_some());
        assert!(root.find_subnode("/test/node/one").is_some());
        assert!(root.find_subnode("/test/node/two").is_some());
        assert!(root.find_subnode("/test/othernode").is_some());
        assert!(root.find_subnode("/test/othernode/one").is_some());
        assert!(root.find_subnode("/nonexistent").is_none());
    }

    #[test]
    fn test_node_serialization() {
        let node = OSCQueryNode::new("/test")
            .with_access(OSCAccess::ReadWrite)
            .with_value(vec![OscValue::Int(42), OscValue::Float(3.14)])
            .with_description("a test node");

        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["FULL_PATH"], "/test");
        assert_eq!(json["TYPE"], "if");
        assert_eq!(json["ACCESS"], 3);
        assert_eq!(json["VALUE"][0], 42);
        assert_eq!(json["DESCRIPTION"], "a test node");
    }

    #[test]
    fn test_node_json_round_trip() {
        let original = OSCQueryNode::new("/test/path")
            .with_access(OSCAccess::ReadOnly)
            .with_value(vec![OscValue::Bool(true), OscValue::String("hello".into())])
            .with_description("round trip test");

        let json_str = serde_json::to_string(&original).unwrap();
        let deserialized: OSCQueryNode = serde_json::from_str(&json_str).unwrap();

        assert_eq!(deserialized.full_path.as_deref(), Some("/test/path"));
        assert_eq!(deserialized.osc_type.as_deref(), Some("Ts"));
        assert_eq!(deserialized.access, Some(OSCAccess::ReadOnly));
        assert_eq!(
            deserialized.value,
            Some(vec![
                OscValue::Bool(true),
                OscValue::String("hello".into())
            ])
        );
        assert_eq!(deserialized.description.as_deref(), Some("round trip test"));
    }

    #[test]
    fn test_tree_serialization_with_contents() {
        let mut root = OSCQueryNode::new("/").with_description("root node");
        root.add_child_node(
            OSCQueryNode::new("/test")
                .with_access(OSCAccess::ReadWrite)
                .with_value(vec![OscValue::Int(1)]),
        );

        let json = serde_json::to_value(&root).unwrap();
        assert!(json["CONTENTS"]["test"].is_object());
        assert_eq!(json["CONTENTS"]["test"]["FULL_PATH"], "/test");
    }

    #[test]
    fn test_type_tag_conversion() {
        let values = vec![
            OscValue::Int(1),
            OscValue::Float(2.0),
            OscValue::Bool(true),
            OscValue::String("s".into()),
        ];
        assert_eq!(osc_type_to_tags(&values), "ifTs");
    }

    #[test]
    fn test_tags_to_type_chars_skips_whitespace() {
        assert_eq!(tags_to_type_chars("i f T s"), vec!['i', 'f', 'T', 's']);
        assert_eq!(tags_to_type_chars("ifTs"), vec!['i', 'f', 'T', 's']);
    }

    #[test]
    fn test_host_info_serialization() {
        let mut extensions = HashMap::new();
        extensions.insert("ACCESS".to_owned(), serde_json::Value::Bool(true));
        extensions.insert("VALUE".to_owned(), serde_json::Value::Bool(true));

        let hi = OSCHostInfo {
            name: "TestService".to_owned(),
            osc_ip: Some("127.0.0.1".to_owned()),
            osc_port: Some(9000),
            osc_transport: Some("UDP".to_owned()),
            extensions,
        };

        let json = serde_json::to_value(&hi).unwrap();
        assert_eq!(json["NAME"], "TestService");
        assert_eq!(json["OSC_IP"], "127.0.0.1");
        assert_eq!(json["OSC_PORT"], 9000);
        assert_eq!(json["OSC_TRANSPORT"], "UDP");
        assert_eq!(json["EXTENSIONS"]["ACCESS"], true);
    }

    #[test]
    fn test_with_osc_type() {
        let node = OSCQueryNode::new("/test").with_osc_type("ff").with_access(OSCAccess::ReadWrite);
        assert_eq!(node.osc_type.as_deref(), Some("ff"));
        assert!(node.value.is_none());
    }

    #[test]
    fn test_default_values_for_type_tags() {
        let vals = default_values_for_type_tags("ifTsd").unwrap();
        assert_eq!(vals.len(), 5);
        assert_eq!(vals[0], OscValue::Int(0));
        assert_eq!(vals[1], OscValue::Float(0.0));
        assert_eq!(vals[2], OscValue::Bool(false));
        assert_eq!(vals[3], OscValue::String(String::new()));
        assert_eq!(vals[4], OscValue::Double(0.0));
    }

    #[test]
    fn test_default_values_for_unknown_tag() {
        let result = default_values_for_type_tags("x");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown OSC type tag"));
    }

    #[test]
    fn test_host_info_round_trip() {
        let mut extensions = HashMap::new();
        extensions.insert("TYPE".to_owned(), serde_json::Value::Bool(true));

        let original = OSCHostInfo {
            name: "Test".to_owned(),
            osc_ip: Some("192.168.1.1".to_owned()),
            osc_port: Some(8080),
            osc_transport: Some("TCP".to_owned()),
            extensions,
        };

        let json_str = serde_json::to_string(&original).unwrap();
        let deserialized: OSCHostInfo = serde_json::from_str(&json_str).unwrap();

        assert_eq!(deserialized.name, "Test");
        assert_eq!(deserialized.osc_ip.as_deref(), Some("192.168.1.1"));
        assert_eq!(deserialized.osc_port, Some(8080));
        assert_eq!(deserialized.osc_transport.as_deref(), Some("TCP"));
        assert_eq!(
            deserialized.extensions.get("TYPE"),
            Some(&serde_json::Value::Bool(true))
        );
    }
}
