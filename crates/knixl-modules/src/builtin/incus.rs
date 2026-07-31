//! `incus`: an Incus host. A built-in module; see docs/04-template-grammar.md for why.
use crate::builtin::host::unit_default;
use crate::{
    Child, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, Unit, ValueTy,
};
use kdl::KdlNode;
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr};
use knixl_kdl::{children_named, first_arg_str};
use std::collections::BTreeMap;

const DEFAULT_API_PORT: i128 = 8443;

pub struct Incus {
    schema: NodeSchema,
}

impl Incus {
    pub fn new() -> Self {
        Self { schema: schema() }
    }
}
impl Default for Incus {
    fn default() -> Self {
        Self::new()
    }
}

// ---- internal representation ----

#[derive(Debug, Clone, PartialEq)]
struct StoragePool {
    name: String,
    driver: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Network {
    name: String,
    ty: String,
    ipv4: String,
    ipv4_nat: String,
    ipv6: Option<String>,
    ipv6_nat: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Profile {
    name: String,
    pool: String,
    network: String,
}

/// How (if at all) to expose the remote API. `core.https_address` is either a static value
/// written straight into the preseed, or resolved at runtime from an interface's address by a
/// oneshot (the tailnet-bind pattern). At most one form is allowed.
#[derive(Debug, Clone, PartialEq)]
enum Listener {
    None,
    Static(String),
    FromInterface(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Firewall {
    trust_interfaces: Vec<String>,
    open_api_on: Vec<String>,
}

struct Config {
    ui: bool,
    pools: Vec<StoragePool>,
    networks: Vec<Network>,
    profiles: Vec<Profile>,
    listener: Listener,
    firewall: Firewall,
}

impl Module for Incus {
    fn id(&self) -> ModuleId {
        ModuleId {
            name: "incus".into(),
            version: "1.0.0".parse().unwrap(),
        }
    }
    fn node_name(&self) -> &str {
        "incus"
    }
    fn schema(&self) -> &NodeSchema {
        &self.schema
    }
    fn lower(&self, node: &KdlNode, _ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let cfg = parse(node)?;
        Ok(LowerOutput::units(emit(&cfg)))
    }
}

// ---- parse ----

fn parse(node: &KdlNode) -> Result<Config, LowerError> {
    let ui = children_named(node, "ui")
        .next()
        .map(|n| {
            n.entries()
                .iter()
                .find(|e| e.name().is_none())
                .and_then(|e| e.value().as_bool())
                .unwrap_or(true)
        })
        .unwrap_or(false);

    let mut pools = Vec::new();
    for n in children_named(node, "storage-pool") {
        let name = first_arg_str(n)
            .ok_or_else(|| LowerError::Other("`storage-pool` needs a name".into()))?;
        pools.push(StoragePool {
            driver: req_prop(n, "driver", &format!("storage-pool `{name}`"))?,
            source: req_prop(n, "source", &format!("storage-pool `{name}`"))?,
            name,
        });
    }

    let mut networks = Vec::new();
    for n in children_named(node, "network") {
        let name =
            first_arg_str(n).ok_or_else(|| LowerError::Other("`network` needs a name".into()))?;
        let ctx = format!("network `{name}`");
        networks.push(Network {
            ty: req_prop(n, "type", &ctx)?,
            ipv4: req_prop(n, "ipv4", &ctx)?,
            ipv4_nat: req_prop(n, "nat", &ctx)?,
            ipv6: opt_prop(n, "ipv6"),
            ipv6_nat: opt_prop(n, "ipv6-nat"),
            name,
        });
    }

    let mut profiles = Vec::new();
    for n in children_named(node, "profile") {
        let name =
            first_arg_str(n).ok_or_else(|| LowerError::Other("`profile` needs a name".into()))?;
        let ctx = format!("profile `{name}`");
        profiles.push(Profile {
            pool: req_prop(n, "pool", &ctx)?,
            network: req_prop(n, "network", &ctx)?,
            name,
        });
    }

    let static_addr = at_most_one_arg(node, "https-address")?;
    let from_iface = at_most_one_arg(node, "https-address-from-interface")?;
    let listener = match (static_addr, from_iface) {
        (Some(_), Some(_)) => {
            return Err(LowerError::Other(
                "`https-address` and `https-address-from-interface` are mutually exclusive".into(),
            ))
        }
        (Some(a), None) => Listener::Static(a),
        (None, Some(i)) => Listener::FromInterface(i),
        (None, None) => Listener::None,
    };

    let firewall = match children_named(node, "firewall").next() {
        Some(fw) => Firewall {
            trust_interfaces: children_named(fw, "trust-interface")
                .filter_map(first_arg_str)
                .collect(),
            open_api_on: children_named(fw, "open-api-on")
                .filter_map(first_arg_str)
                .collect(),
        },
        None => Firewall::default(),
    };

    Ok(Config {
        ui,
        pools,
        networks,
        profiles,
        listener,
        firewall,
    })
}

fn req_prop(n: &KdlNode, key: &str, ctx: &str) -> Result<String, LowerError> {
    n.get(key)
        .and_then(|v| v.as_string())
        .map(str::to_string)
        .ok_or_else(|| LowerError::missing(&format!("{ctx}.{key}")))
}

fn opt_prop(n: &KdlNode, key: &str) -> Option<String> {
    n.get(key).and_then(|v| v.as_string()).map(str::to_string)
}

/// The single arg of an at-most-one child node (e.g. `https-address "1.2.3.4:8443"`).
fn at_most_one_arg(node: &KdlNode, name: &str) -> Result<Option<String>, LowerError> {
    let matching: Vec<&KdlNode> = children_named(node, name).collect();
    match matching.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(first_arg_str(one).ok_or_else(|| {
            LowerError::Other(format!("`{name}` needs a value"))
        })?)),
        _ => Err(LowerError::Other(format!(
            "at most one `{name}` is allowed"
        ))),
    }
}

/// The API port advertised for the firewall: parsed from a static `host:port` address if one is
/// set, otherwise the incus default 8443.
fn api_port(listener: &Listener) -> i128 {
    if let Listener::Static(addr) = listener {
        if let Some((_, port)) = addr.rsplit_once(':') {
            if let Ok(p) = port.parse::<i128>() {
                return p;
            }
        }
    }
    DEFAULT_API_PORT
}

// ---- emit ----

fn s(v: &str) -> NixExpr {
    NixExpr::Str(v.to_string())
}

fn ident_set(entries: Vec<(&str, NixExpr)>) -> NixExpr {
    let mut m: BTreeMap<AttrKey, NixExpr> = BTreeMap::new();
    for (k, v) in entries {
        m.insert(AttrKey::Ident(k.to_string()), v);
    }
    NixExpr::AttrSet(m)
}

fn quoted_set(entries: Vec<(&str, NixExpr)>) -> NixExpr {
    let mut m: BTreeMap<AttrKey, NixExpr> = BTreeMap::new();
    for (k, v) in entries {
        m.insert(AttrKey::Quoted(k.to_string()), v);
    }
    NixExpr::AttrSet(m)
}

fn idents(segs: &[&str]) -> AttrPath {
    AttrPath(segs.iter().map(|s| AttrKey::Ident((*s).into())).collect())
}

fn assign(path: AttrPath, value: NixExpr) -> Assignment {
    Assignment {
        path,
        value,
        priority: None,
        condition: None,
        doc: None,
    }
}

fn emit(cfg: &Config) -> Vec<Unit> {
    let mut units = Vec::new();

    units.push(unit_default(assign(
        idents(&["virtualisation", "incus", "enable"]),
        NixExpr::Bool(true),
    )));
    if cfg.ui {
        units.push(unit_default(assign(
            idents(&["virtualisation", "incus", "ui", "enable"]),
            NixExpr::Bool(true),
        )));
    }

    if !cfg.pools.is_empty() {
        let items = cfg
            .pools
            .iter()
            .map(|p| {
                ident_set(vec![
                    ("name", s(&p.name)),
                    ("driver", s(&p.driver)),
                    ("config", ident_set(vec![("source", s(&p.source))])),
                ])
            })
            .collect();
        units.push(unit_default(assign(
            idents(&["virtualisation", "incus", "preseed", "storage_pools"]),
            NixExpr::List(items),
        )));
    }

    if !cfg.networks.is_empty() {
        let items = cfg
            .networks
            .iter()
            .map(|n| {
                let mut config = vec![("ipv4.address", s(&n.ipv4)), ("ipv4.nat", s(&n.ipv4_nat))];
                if let Some(a) = &n.ipv6 {
                    config.push(("ipv6.address", s(a)));
                }
                if let Some(nat) = &n.ipv6_nat {
                    config.push(("ipv6.nat", s(nat)));
                }
                ident_set(vec![
                    ("name", s(&n.name)),
                    ("type", s(&n.ty)),
                    ("config", quoted_set(config)),
                ])
            })
            .collect();
        units.push(unit_default(assign(
            idents(&["virtualisation", "incus", "preseed", "networks"]),
            NixExpr::List(items),
        )));
    }

    if !cfg.profiles.is_empty() {
        let items = cfg
            .profiles
            .iter()
            .map(|p| {
                ident_set(vec![
                    ("name", s(&p.name)),
                    (
                        "devices",
                        ident_set(vec![
                            (
                                "root",
                                ident_set(vec![
                                    ("path", s("/")),
                                    ("pool", s(&p.pool)),
                                    ("type", s("disk")),
                                ]),
                            ),
                            (
                                "eth0",
                                ident_set(vec![
                                    ("name", s("eth0")),
                                    ("network", s(&p.network)),
                                    ("type", s("nic")),
                                ]),
                            ),
                        ]),
                    ),
                ])
            })
            .collect();
        units.push(unit_default(assign(
            idents(&["virtualisation", "incus", "preseed", "profiles"]),
            NixExpr::List(items),
        )));
    }

    match &cfg.listener {
        Listener::None => {}
        Listener::Static(addr) => {
            units.push(unit_default(assign(
                AttrPath(vec![
                    AttrKey::Ident("virtualisation".into()),
                    AttrKey::Ident("incus".into()),
                    AttrKey::Ident("preseed".into()),
                    AttrKey::Ident("config".into()),
                    AttrKey::Quoted("core.https_address".into()),
                ]),
                s(addr),
            )));
        }
        Listener::FromInterface(iface) => {
            units.push(unit_default(assign(
                AttrPath(vec![
                    AttrKey::Ident("systemd".into()),
                    AttrKey::Ident("services".into()),
                    AttrKey::Quoted("incus-https-address".into()),
                ]),
                https_address_oneshot(iface, api_port(&cfg.listener)),
            )));
        }
    }

    if !cfg.firewall.trust_interfaces.is_empty() {
        units.push(unit_default(assign(
            idents(&["networking", "firewall", "trustedInterfaces"]),
            NixExpr::List(cfg.firewall.trust_interfaces.iter().map(|i| s(i)).collect()),
        )));
    }
    for iface in &cfg.firewall.open_api_on {
        units.push(unit_default(assign(
            AttrPath(vec![
                AttrKey::Ident("networking".into()),
                AttrKey::Ident("firewall".into()),
                AttrKey::Ident("interfaces".into()),
                AttrKey::Quoted(iface.clone()),
                AttrKey::Ident("allowedTCPPorts".into()),
            ]),
            NixExpr::List(vec![NixExpr::Int(api_port(&cfg.listener))]),
        )));
    }

    units
}

/// A systemd oneshot that, after the daemon is up, resolves `iface`'s IPv4 address and sets
/// `core.https_address` to `<addr>:<port>`. The binaries come from the unit's `path`, so the
/// script itself is plain shell with no Nix antiquotation.
fn https_address_oneshot(iface: &str, port: i128) -> NixExpr {
    let script = format!(
        "addr=$(ip -4 -o addr show dev {iface} scope global | awk '{{print $4}}' | cut -d/ -f1 | head -n1)\nif [ -n \"$addr\" ]; then\n  incus config set core.https_address \"$addr:{port}\"\nfi\n"
    );
    ident_set(vec![
        (
            "description",
            s(&format!("Bind the Incus HTTPS API to the {iface} address")),
        ),
        (
            "after",
            NixExpr::List(vec![s("incus.service"), s("network-online.target")]),
        ),
        ("requires", NixExpr::List(vec![s("incus.service")])),
        ("wants", NixExpr::List(vec![s("network-online.target")])),
        ("wantedBy", NixExpr::List(vec![s("multi-user.target")])),
        (
            "path",
            NixExpr::List(vec![
                pkg("iproute2"),
                pkg("gawk"),
                pkg("coreutils"),
                pkg("incus"),
            ]),
        ),
        ("serviceConfig", ident_set(vec![("Type", s("oneshot"))])),
        ("script", NixExpr::IndentStr(script)),
    ])
}

fn pkg(name: &str) -> NixExpr {
    NixExpr::Select(
        Box::new(NixExpr::Ref("pkgs".into())),
        vec![name.to_string()],
    )
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "An Incus host: enable, the web UI, the daemon preseed, an optional API \
                  listener, and host-firewall integration."
            .into(),
        args: vec![],
        props: vec![],
        children: vec![
            node_child("ui", ValueTy::Bool, "Enable the Incus web UI."),
            node_child(
                "https-address",
                ValueTy::Str,
                "Static core.https_address, e.g. \"10.0.0.1:8443\".",
            ),
            node_child(
                "https-address-from-interface",
                ValueTy::Str,
                "Bind core.https_address to this interface's IPv4 at runtime via a oneshot.",
            ),
            node_child(
                "firewall",
                ValueTy::Node,
                "Host firewall integration: trust-interface and open-api-on children.",
            ),
            node_child("storage-pool", ValueTy::Node, "A preseed storage pool."),
            node_child("network", ValueTy::Node, "A preseed network."),
            node_child("profile", ValueTy::Node, "A preseed profile."),
        ],
        open_children: false,
    }
}

fn node_child(name: &str, ty: ValueTy, doc: &str) -> Child {
    Child {
        name: name.into(),
        ty,
        required: false,
        repeated: true,
        delegate: false,
        doc: doc.into(),
        args: vec![],
        props: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Registry, Scope};

    fn node(src: &str) -> KdlNode {
        src.parse::<kdl::KdlDocument>()
            .unwrap()
            .nodes()
            .first()
            .unwrap()
            .clone()
    }

    fn lower_ok(src: &str) -> Vec<Unit> {
        let m = Incus::new();
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(
            Scope {
                host: "vmhost".into(),
            },
            &reg,
            &mut diags,
            vec![],
        );
        m.lower(&node(src), &mut ctx).expect("lower ok").units
    }

    fn lower_err(src: &str) -> String {
        let m = Incus::new();
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(
            Scope {
                host: "vmhost".into(),
            },
            &reg,
            &mut diags,
            vec![],
        );
        match m.lower(&node(src), &mut ctx) {
            Ok(_) => panic!("expected an error, got Ok"),
            Err(e) => format!("{e}"),
        }
    }

    fn find<'a>(units: &'a [Unit], path: &str) -> Option<&'a NixExpr> {
        units
            .iter()
            .map(|u| &u.assignment)
            .find(|a| {
                a.path
                    .0
                    .iter()
                    .map(|k| match k {
                        AttrKey::Ident(s) => s.clone(),
                        AttrKey::Quoted(s) => format!("\"{s}\""),
                    })
                    .collect::<Vec<_>>()
                    .join(".")
                    == path
            })
            .map(|a| &a.value)
    }

    #[test]
    fn base_preseed_matches_the_declarative_shape() {
        let units = lower_ok(
            "incus {\n    ui\n    storage-pool \"default\" driver=\"zfs\" source=\"rpool/incus\"\n    network \"incusbr0\" type=\"bridge\" ipv4=\"auto\" nat=\"true\"\n    profile \"default\" pool=\"default\" network=\"incusbr0\"\n}",
        );
        assert!(matches!(
            find(&units, "virtualisation.incus.enable"),
            Some(NixExpr::Bool(true))
        ));
        assert!(matches!(
            find(&units, "virtualisation.incus.ui.enable"),
            Some(NixExpr::Bool(true))
        ));
        // networks list holds one network attrset with quoted ipv4 config keys
        let NixExpr::List(nets) = find(&units, "virtualisation.incus.preseed.networks").unwrap()
        else {
            panic!("networks not a list")
        };
        assert_eq!(nets.len(), 1);
        let NixExpr::AttrSet(net) = &nets[0] else {
            panic!()
        };
        let NixExpr::AttrSet(config) = net.get(&AttrKey::Ident("config".into())).unwrap() else {
            panic!()
        };
        assert!(config.contains_key(&AttrKey::Quoted("ipv4.address".into())));
        assert!(!config.contains_key(&AttrKey::Quoted("ipv6.address".into())));
    }

    #[test]
    fn optional_ipv6_is_emitted_only_when_given() {
        let units = lower_ok(
            "incus {\n    network \"incusbr0\" type=\"bridge\" ipv4=\"10.0.0.1/24\" nat=\"true\" ipv6=\"fd42::1/64\" ipv6-nat=\"true\"\n}",
        );
        let NixExpr::List(nets) = find(&units, "virtualisation.incus.preseed.networks").unwrap()
        else {
            panic!()
        };
        let NixExpr::AttrSet(net) = &nets[0] else {
            panic!()
        };
        let NixExpr::AttrSet(config) = net.get(&AttrKey::Ident("config".into())).unwrap() else {
            panic!()
        };
        assert!(matches!(
            config.get(&AttrKey::Quoted("ipv6.address".into())),
            Some(NixExpr::Str(s)) if s == "fd42::1/64"
        ));
        assert!(matches!(
            config.get(&AttrKey::Quoted("ipv6.nat".into())),
            Some(NixExpr::Str(s)) if s == "true"
        ));
    }

    #[test]
    fn static_https_address_emits_preseed_config() {
        let units = lower_ok("incus {\n    https-address \"10.0.0.1:8443\"\n}");
        assert!(matches!(
            find(&units, "virtualisation.incus.preseed.config.\"core.https_address\""),
            Some(NixExpr::Str(s)) if s == "10.0.0.1:8443"
        ));
    }

    #[test]
    fn https_address_from_interface_emits_a_oneshot() {
        let units = lower_ok("incus {\n    https-address-from-interface \"tailscale0\"\n}");
        let svc =
            find(&units, "systemd.services.\"incus-https-address\"").expect("oneshot present");
        let NixExpr::AttrSet(m) = svc else { panic!() };
        let NixExpr::IndentStr(script) = m.get(&AttrKey::Ident("script".into())).unwrap() else {
            panic!("script not an indent string")
        };
        assert!(script.contains("dev tailscale0"));
        assert!(script.contains("incus config set core.https_address"));
        assert!(script.contains("$addr:8443"));
        // binaries are on the unit path, not antiquoted into the script
        assert!(!script.contains("${"));
        assert!(m.contains_key(&AttrKey::Ident("path".into())));
    }

    #[test]
    fn static_and_from_interface_are_mutually_exclusive() {
        let e = lower_err(
            "incus {\n    https-address \"10.0.0.1:8443\"\n    https-address-from-interface \"tailscale0\"\n}",
        );
        assert!(e.contains("mutually exclusive"), "got: {e}");
    }

    #[test]
    fn firewall_emits_trusted_interfaces_and_per_iface_ports() {
        let units = lower_ok(
            "incus {\n    https-address \"10.0.0.1:9000\"\n    firewall {\n        trust-interface \"incusbr0\"\n        open-api-on \"tailscale0\"\n    }\n}",
        );
        let NixExpr::List(trusted) = find(&units, "networking.firewall.trustedInterfaces").unwrap()
        else {
            panic!()
        };
        assert!(matches!(&trusted[0], NixExpr::Str(s) if s == "incusbr0"));
        // the port follows the static address (9000), not the 8443 default
        let NixExpr::List(ports) = find(
            &units,
            "networking.firewall.interfaces.\"tailscale0\".allowedTCPPorts",
        )
        .unwrap() else {
            panic!()
        };
        assert!(matches!(&ports[0], NixExpr::Int(9000)));
    }

    #[test]
    fn missing_required_network_prop_errors() {
        let e = lower_err("incus {\n    network \"n\" type=\"bridge\" ipv4=\"auto\"\n}");
        assert!(e.contains("nat"), "got: {e}");
    }
}
