//! Declarative modules: a `knixl-module.kdl` manifest (schema, emit template, migrations)
//! loaded as a `Module`. The emit block is the template language in `template`.

use crate::template::EmitTemplate;
use crate::{Child, Field, LowerError, LowerOutput, Module, ModuleId, NodeSchema, ValueTy};
use knixl_kdl::first_arg_str;

pub struct DeclarativeModule {
    id: ModuleId,
    node_name: String,
    schema: NodeSchema,
    template: EmitTemplate,
    migrations: Vec<crate::MigrationNote>,
}

impl DeclarativeModule {
    pub fn from_kdl(doc: &kdl::KdlDocument, source: &std::path::Path) -> Result<Self, LowerError> {
        let where_ = || source.display().to_string();
        let module = doc
            .nodes()
            .iter()
            .find(|n| n.name().value() == "module")
            .ok_or_else(|| LowerError::Other(format!("{}: missing `module` node", where_())))?;

        let name = prop_str(module, "name")
            .ok_or_else(|| LowerError::Other(format!("{}: module missing `name`", where_())))?;
        let version_str = prop_str(module, "version")
            .ok_or_else(|| LowerError::Other(format!("{}: module missing `version`", where_())))?;
        let version = version_str.parse().map_err(|e| {
            LowerError::Other(format!("{}: bad version `{version_str}`: {e}", where_()))
        })?;

        let body = module
            .children()
            .ok_or_else(|| LowerError::Other(format!("{}: empty module", where_())))?;

        let mut summary = String::new();
        let mut node_name = None;
        let mut schema = None;
        let mut template = None;
        let mut migrations = Vec::new();
        for child in body.nodes() {
            match child.name().value() {
                "summary" => summary = first_arg_str(child).unwrap_or_default(),
                "claims-node" => node_name = first_arg_str(child),
                "schema" => schema = Some(parse_schema_block(child)?),
                "emit" => template = Some(EmitTemplate::parse(child.children())?),
                "migrations" => migrations = parse_migrations(child, &where_)?,
                other => {
                    return Err(LowerError::Other(format!(
                        "{}: unexpected `{other}` in module",
                        where_()
                    )))
                }
            }
        }

        let node_name = node_name.ok_or_else(|| {
            LowerError::Other(format!("{}: module missing `claims-node`", where_()))
        })?;
        let mut schema = schema
            .ok_or_else(|| LowerError::Other(format!("{}: module missing `schema`", where_())))?;
        schema.summary = summary;
        let template = template
            .ok_or_else(|| LowerError::Other(format!("{}: module missing `emit`", where_())))?;

        template
            .check(&schema)
            .map_err(|e| LowerError::Other(format!("{}: {e}", where_())))?;

        Ok(DeclarativeModule {
            id: ModuleId { name, version },
            node_name,
            schema,
            template,
            migrations,
        })
    }
}

/// Parse a `migrations` block: each `to "<version>"` child holds one or more `note` lines.
fn parse_migrations(
    node: &kdl::KdlNode,
    where_: &impl Fn() -> String,
) -> Result<Vec<crate::MigrationNote>, LowerError> {
    let mut steps = Vec::new();
    let Some(body) = node.children() else {
        return Ok(steps);
    };
    for step in body.nodes() {
        if step.name().value() != "to" {
            return Err(LowerError::Other(format!(
                "{}: unexpected `{}` in migrations (expected `to`)",
                where_(),
                step.name().value()
            )));
        }
        let ver_str = first_arg_str(step).ok_or_else(|| {
            LowerError::Other(format!("{}: migration `to` needs a version", where_()))
        })?;
        let to = ver_str.parse().map_err(|e| {
            LowerError::Other(format!(
                "{}: bad migration version `{ver_str}`: {e}",
                where_()
            ))
        })?;
        let notes = step
            .children()
            .map(|b| {
                b.nodes()
                    .iter()
                    .filter(|n| n.name().value() == "note")
                    .filter_map(first_arg_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        steps.push(crate::MigrationNote { to, notes });
    }
    Ok(steps)
}

// ---- KDL parsing of the schema and emit blocks ----

fn parse_schema_block(schema_node: &kdl::KdlNode) -> Result<NodeSchema, LowerError> {
    let mut args = Vec::new();
    let mut props = Vec::new();
    let mut children = Vec::new();
    if let Some(body) = schema_node.children() {
        for n in body.nodes() {
            match n.name().value() {
                "arg" => args.push(parse_field(n)?),
                "prop" => props.push(parse_field(n)?),
                "child" => children.push(parse_child(n)?),
                other => return Err(LowerError::Other(format!("unexpected `{other}` in schema"))),
            }
        }
    }
    // Declarative modules are leaves: they read their own subtree, never delegate.
    Ok(NodeSchema {
        summary: String::new(),
        args,
        props,
        children,
        open_children: false,
    })
}

fn parse_field(n: &kdl::KdlNode) -> Result<Field, LowerError> {
    let name =
        first_arg_str(n).ok_or_else(|| LowerError::Other("schema field missing name".into()))?;
    Ok(Field {
        name,
        ty: ty_from(prop_str(n, "type").as_deref()),
        required: prop_bool(n, "required").unwrap_or(false),
        doc: prop_str(n, "doc").unwrap_or_default(),
    })
}

fn parse_child(n: &kdl::KdlNode) -> Result<Child, LowerError> {
    let name =
        first_arg_str(n).ok_or_else(|| LowerError::Other("schema child missing name".into()))?;
    let required = prop_bool(n, "required").unwrap_or(false);
    let repeated = prop_bool(n, "repeated").unwrap_or(false);
    let doc = prop_str(n, "doc").unwrap_or_default();

    // A child with a block is structured: its own args/props form a Scope.
    if let Some(body) = n.children() {
        let mut args = Vec::new();
        let mut props = Vec::new();
        for s in body.nodes() {
            match s.name().value() {
                "arg" => args.push(parse_field(s)?),
                "prop" => props.push(parse_field(s)?),
                other => {
                    return Err(LowerError::Other(format!(
                        "unexpected `{other}` in structured child"
                    )))
                }
            }
        }
        Ok(Child {
            name,
            ty: ValueTy::Node,
            required,
            repeated,
            delegate: false,
            doc,
            args,
            props,
        })
    } else {
        Ok(Child {
            name,
            ty: ty_from(prop_str(n, "type").as_deref()),
            required,
            repeated,
            delegate: false,
            doc,
            args: Vec::new(),
            props: Vec::new(),
        })
    }
}

fn ty_from(ty: Option<&str>) -> ValueTy {
    match ty {
        Some("bool") => ValueTy::Bool,
        Some("int") => ValueTy::Int,
        _ => ValueTy::Str,
    }
}

impl Module for DeclarativeModule {
    fn id(&self) -> ModuleId {
        self.id.clone()
    }
    fn node_name(&self) -> &str {
        &self.node_name
    }
    fn kind(&self) -> crate::ModuleKind {
        crate::ModuleKind::Declarative
    }
    fn schema(&self) -> &NodeSchema {
        &self.schema
    }
    fn lower(
        &self,
        node: &kdl::KdlNode,
        ctx: &mut crate::LowerCtx,
    ) -> Result<LowerOutput, LowerError> {
        self.template
            .lower(&self.schema, node, ctx.secrets_backend())
    }
    fn migration_notes(&self, from: &semver::Version, to: &semver::Version) -> Vec<String> {
        crate::notes_in_range(&self.migrations, from, to)
    }
}

fn prop_str(node: &kdl::KdlNode, key: &str) -> Option<String> {
    node.get(key)
        .and_then(|v| v.as_string())
        .map(str::to_string)
}

fn prop_bool(node: &kdl::KdlNode, key: &str) -> Option<bool> {
    node.get(key).and_then(|v| v.as_bool())
}

#[cfg(test)]
mod tests {
    use super::*;
    use knixl_ir::{Assignment, AttrKey, NixExpr};

    fn load_web_service() -> DeclarativeModule {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/stdlib/web-service/knixl-module.kdl"
        ))
        .expect("read manifest");
        let doc = src.parse::<kdl::KdlDocument>().expect("parse manifest");
        DeclarativeModule::from_kdl(&doc, std::path::Path::new("web-service")).expect("from_kdl")
    }

    fn node(src: &str) -> kdl::KdlNode {
        src.parse::<kdl::KdlDocument>()
            .unwrap()
            .nodes()
            .first()
            .unwrap()
            .clone()
    }

    #[test]
    fn migration_notes_apply_only_to_crossed_steps() {
        use crate::Module;
        let m = load_web_service();
        let v = |s: &str| s.parse::<semver::Version>().unwrap();

        // Fresh install crossing both steps: both notes, in ascending order.
        let all = m.migration_notes(&v("1.0.0"), &v("1.2.0"));
        assert_eq!(all.len(), 2, "1.0.0 -> 1.2.0 crosses both steps: {all:?}");
        assert!(all[0].contains("enableACME"), "1.1.0 note first: {all:?}");
        assert!(
            all[1].contains("serverAliases"),
            "1.2.0 note second: {all:?}"
        );

        // Only the final step is crossed.
        let one = m.migration_notes(&v("1.1.0"), &v("1.2.0"));
        assert_eq!(one, vec![all[1].clone()]);

        // No move, no notes.
        assert!(m.migration_notes(&v("1.2.0"), &v("1.2.0")).is_empty());
    }

    fn lower(module: &DeclarativeModule, n: &kdl::KdlNode) -> LowerOutput {
        let mut diags = Vec::new();
        let mut ctx = crate::LowerCtx::new(crate::Scope { host: "web".into() }, &mut diags, vec![]);
        module.lower(n, &mut ctx).expect("lower")
    }

    fn path_str(a: &Assignment) -> String {
        a.path
            .0
            .iter()
            .map(|k| match k {
                AttrKey::Ident(s) => s.clone(),
                AttrKey::Quoted(s) => format!("\"{s}\""),
            })
            .collect::<Vec<_>>()
            .join(".")
    }

    fn find<'a>(out: &'a LowerOutput, path: &str) -> Option<&'a NixExpr> {
        out.units
            .iter()
            .map(|u| &u.assignment)
            .find(|a| path_str(a) == path)
            .map(|a| &a.value)
    }

    #[test]
    fn web_service_expands_the_manifest() {
        let module = load_web_service();
        assert_eq!(module.node_name(), "web-service");

        let n = node("web-service \"example.com\" {\n    upstream \"http://127.0.0.1:3000\"\n    acme email=\"ops@example.com\"\n    hardened #true\n}");
        let out = lower(&module, &n);

        assert!(matches!(
            find(&out, "services.nginx.enable"),
            Some(NixExpr::Bool(true))
        ));

        // {host} interpolated into the path
        assert!(find(&out, "services.nginx.virtualHosts.\"example.com\".forceSSL").is_some());

        // {upstream} interpolated into the value; "/" is a quoted segment
        match find(
            &out,
            "services.nginx.virtualHosts.\"example.com\".locations.\"/\".proxyPass",
        ) {
            Some(NixExpr::Str(s)) => assert_eq!(s, "http://127.0.0.1:3000"),
            other => panic!("proxyPass = {other:?}"),
        }

        // {acme.email} resolves through the acme Scope
        match find(&out, "security.acme.certs.\"example.com\".email") {
            Some(NixExpr::Str(s)) => assert_eq!(s, "ops@example.com"),
            other => panic!("email = {other:?}"),
        }

        // when-flag hardened=true includes the indent-str block
        match find(
            &out,
            "services.nginx.virtualHosts.\"example.com\".locations.\"/\".extraConfig",
        ) {
            Some(NixExpr::IndentStr(s)) => assert!(s.contains("X-Frame-Options")),
            other => panic!("extraConfig = {other:?}"),
        }

        // collect with no aliases => empty list
        match find(
            &out,
            "services.nginx.virtualHosts.\"example.com\".serverAliases",
        ) {
            Some(NixExpr::List(items)) => assert!(items.is_empty()),
            other => panic!("serverAliases = {other:?}"),
        }
    }

    #[test]
    fn web_service_handles_repeats_and_a_disabled_flag() {
        let module = load_web_service();
        let n = node("web-service \"ex.com\" {\n    upstream \"u\"\n    acme email=\"e\"\n    alias \"a.com\"\n    alias \"b.com\"\n    location \"/api\" upstream=\"http://up:4000\"\n}");
        let out = lower(&module, &n);

        // collect folds both aliases in source order
        match find(&out, "services.nginx.virtualHosts.\"ex.com\".serverAliases") {
            Some(NixExpr::List(items)) => assert_eq!(items.len(), 2),
            other => panic!("serverAliases = {other:?}"),
        }

        // for-each binds loc.match into the path and loc.upstream into the value
        match find(
            &out,
            "services.nginx.virtualHosts.\"ex.com\".locations.\"/api\".proxyPass",
        ) {
            Some(NixExpr::Str(s)) => assert_eq!(s, "http://up:4000"),
            other => panic!("for-each proxyPass = {other:?}"),
        }

        // hardened absent => flag false => no extraConfig emitted
        assert!(find(
            &out,
            "services.nginx.virtualHosts.\"ex.com\".locations.\"/\".extraConfig"
        )
        .is_none());
    }

    #[test]
    fn scalar_value_form_emits_native_bool_int_and_string() {
        // (scalar)"{...}" resolves a bound arg to its native Nix type, unlike "{...}" which
        // always stringifies. A repeated child feeds the loop var.
        let manifest = "module name=\"s\" version=\"0.1.0\" {\n    claims-node \"s\"\n    schema {\n        child \"flag\" repeated=#true {\n            arg \"value\" type=\"bool\" required=#true\n        }\n        child \"num\" repeated=#true {\n            arg \"value\" type=\"int\" required=#true\n        }\n        child \"word\" repeated=#true {\n            arg \"value\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        for-each \"f\" in \"flag\" { set \"a.b\" (scalar)\"{f.value}\" }\n        for-each \"n\" in \"num\" { set \"a.c\" (scalar)\"{n.value}\" }\n        for-each \"w\" in \"word\" { set \"a.d\" (scalar)\"{w.value}\" }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module = DeclarativeModule::from_kdl(&doc, std::path::Path::new("s")).expect("loads");

        let n = node("s {\n    flag #false\n    num 42\n    word \"hi\"\n}");
        let out = lower(&module, &n);

        assert!(
            matches!(find(&out, "a.b"), Some(NixExpr::Bool(false))),
            "bool arg should emit a native Nix bool, got {:?}",
            find(&out, "a.b")
        );
        assert!(
            matches!(find(&out, "a.c"), Some(NixExpr::Int(42))),
            "int arg should emit a native Nix int, got {:?}",
            find(&out, "a.c")
        );
        match find(&out, "a.d") {
            Some(NixExpr::Str(s)) => assert_eq!(s, "hi"),
            other => panic!("string arg should emit a Nix string, got {other:?}"),
        }
    }

    #[test]
    fn dry_check_rejects_a_non_scalar_lookup_in_value_position() {
        // `acme` is a structured child (a Scope), so using {acme} as a value is an error
        // that must surface at load, not at generate.
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n        child \"acme\" {\n            prop \"email\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        set \"services.x.{host}\" \"{acme}\"\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(format!("{err}").contains("not a scalar"), "got: {err}");
    }

    #[test]
    fn dry_check_rejects_for_each_over_a_non_repeated_child() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        child \"upstream\" type=\"string\"\n    }\n    emit {\n        for-each \"u\" in \"upstream\" {\n            set \"a.{u}\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("not a repeated child"),
            "got: {err}"
        );
    }

    #[test]
    fn when_config_stamps_an_interpolated_runtime_condition() {
        use knixl_ir::{Emit, Writer};
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n        arg \"svc\" type=\"string\" required=#true\n    }\n    emit {\n        when-config \"config.services.{svc}.enable\" {\n            set \"services.foo.{host}.enable\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond \"web\" \"postgresql\""));

        let a = out
            .units
            .iter()
            .map(|u| &u.assignment)
            .find(|a| path_str(a) == "services.foo.\"web\".enable")
            .expect("assignment present");

        match &a.condition {
            Some(NixExpr::Raw(r)) => assert_eq!(r.src, "config.services.postgresql.enable"),
            other => panic!("condition = {other:?}"),
        }

        // End-to-end: the IR emit path renders lib.mkIf for this assignment.
        let mut w = Writer::new();
        a.emit(&mut w);
        assert!(
            w.into_string()
                .contains("lib.mkIf (config.services.postgresql.enable)"),
            "expected a lib.mkIf wrapper in the emitted text"
        );
    }

    #[test]
    fn a_set_outside_when_config_has_no_condition() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n    }\n    emit {\n        set \"services.foo.{host}.enable\" #true\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond \"web\""));
        assert!(out.units.iter().all(|u| u.assignment.condition.is_none()));
    }

    #[test]
    fn nested_when_config_and_combines() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n    }\n    emit {\n        when-config \"config.a.enable\" {\n            when-config \"config.b.enable\" {\n                set \"services.foo.{host}.enable\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond \"web\""));
        let a = out
            .units
            .iter()
            .map(|u| &u.assignment)
            .next()
            .expect("one assignment");
        match &a.condition {
            Some(NixExpr::Raw(r)) => assert_eq!(r.src, "(config.a.enable) && (config.b.enable)"),
            other => panic!("condition = {other:?}"),
        }
    }

    #[test]
    fn when_config_inside_for_each_interpolates_the_loop_var() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        child \"item\" type=\"string\" repeated=#true\n    }\n    emit {\n        for-each \"it\" in \"item\" {\n            when-config \"config.services.{it}.enable\" {\n                set \"p.{it}\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(
            &module,
            &node("cond {\n    item \"nginx\"\n    item \"sshd\"\n}"),
        );

        let cond_for = |name: &str| {
            out.units
                .iter()
                .map(|u| &u.assignment)
                .find(|a| path_str(a) == format!("p.\"{name}\""))
                .and_then(|a| match &a.condition {
                    Some(NixExpr::Raw(r)) => Some(r.src.clone()),
                    _ => None,
                })
        };
        assert_eq!(
            cond_for("nginx").as_deref(),
            Some("config.services.nginx.enable")
        );
        assert_eq!(
            cond_for("sshd").as_deref(),
            Some("config.services.sshd.enable")
        );
    }

    #[test]
    fn when_flag_false_drops_a_when_config_body() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        child \"on\" type=\"bool\"\n    }\n    emit {\n        when-flag \"on\" {\n            when-config \"config.a.enable\" {\n                set \"p\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond")); // `on` absent => flag false
        assert!(
            out.units.is_empty(),
            "generation-time gate should drop the body"
        );
    }

    #[test]
    fn when_config_rejects_an_empty_condition() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n    }\n    emit {\n        when-config \"\" {\n            set \"p\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("non-empty condition"),
            "got: {err}"
        );
    }

    #[test]
    fn dry_check_rejects_a_non_scalar_lookup_in_a_condition() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        child \"acme\" {\n            prop \"email\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        when-config \"config.{acme}.enable\" {\n            set \"p\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(format!("{err}").contains("not a scalar"), "got: {err}");
    }

    fn networks_module() -> DeclarativeModule {
        let manifest = "module name=\"net\" version=\"0.1.0\" {\n    claims-node \"net\"\n    schema {\n        child \"network\" repeated=#true {\n            prop \"name\" type=\"string\" required=#true\n            prop \"kind\" type=\"string\" required=#true\n            prop \"ipv4\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        list \"virtualisation.incus.preseed.networks\" from \"network\" {\n            set \"name\" \"{network.name}\"\n            set \"type\" \"{network.kind}\"\n            set \"config.\\\"ipv4.address\\\"\" \"{network.ipv4}\"\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        DeclarativeModule::from_kdl(&doc, std::path::Path::new("net")).expect("loads")
    }

    #[test]
    fn list_folds_a_repeated_child_into_a_list_of_attrsets() {
        let m = networks_module();
        let n = node("net {\n    network name=\"incusbr0\" kind=\"bridge\" ipv4=\"auto\"\n    network name=\"br1\" kind=\"macvlan\" ipv4=\"none\"\n}");
        let out = lower(&m, &n);
        match find(&out, "virtualisation.incus.preseed.networks") {
            Some(NixExpr::List(items)) => {
                assert_eq!(items.len(), 2, "one element per network");
                match &items[0] {
                    NixExpr::AttrSet(map) => {
                        assert!(
                            matches!(map.get(&AttrKey::Ident("name".into())), Some(NixExpr::Str(s)) if s == "incusbr0")
                        );
                        assert!(
                            matches!(map.get(&AttrKey::Ident("type".into())), Some(NixExpr::Str(s)) if s == "bridge")
                        );
                        // nested + quoted key: config."ipv4.address" = "auto"
                        match map.get(&AttrKey::Ident("config".into())) {
                            Some(NixExpr::AttrSet(cfg)) => assert!(
                                matches!(cfg.get(&AttrKey::Quoted("ipv4.address".into())), Some(NixExpr::Str(s)) if s == "auto")
                            ),
                            other => panic!("config = {other:?}"),
                        }
                    }
                    other => panic!("element 0 = {other:?}"),
                }
            }
            other => panic!("networks = {other:?}"),
        }
    }

    #[test]
    fn list_preserves_source_order() {
        let m = networks_module();
        let n = node("net {\n    network name=\"a\" kind=\"bridge\" ipv4=\"1\"\n    network name=\"b\" kind=\"bridge\" ipv4=\"2\"\n}");
        let out = lower(&m, &n);
        let names: Vec<String> = match find(&out, "virtualisation.incus.preseed.networks") {
            Some(NixExpr::List(items)) => items
                .iter()
                .map(|e| match e {
                    NixExpr::AttrSet(m) => match m.get(&AttrKey::Ident("name".into())) {
                        Some(NixExpr::Str(s)) => s.clone(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                })
                .collect(),
            other => panic!("networks = {other:?}"),
        };
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn list_emits_a_list_of_attrsets_and_is_deterministic() {
        use knixl_ir::{Emit, Writer};
        let m = networks_module();
        let n = node("net {\n    network name=\"x\" kind=\"bridge\" ipv4=\"auto\"\n}");
        let render = || {
            let out = lower(&m, &n);
            let a = out
                .units
                .iter()
                .map(|u| &u.assignment)
                .find(|a| path_str(a) == "virtualisation.incus.preseed.networks")
                .unwrap()
                .clone();
            let mut w = Writer::new();
            a.emit(&mut w);
            w.into_string()
        };
        let text = render();
        assert!(text.contains("= ["), "renders a list: {text}");
        assert!(
            text.contains("name = \"x\""),
            "renders the element attr: {text}"
        );
        assert!(
            text.contains("\"ipv4.address\""),
            "renders the quoted nested key: {text}"
        );
        assert_eq!(text, render(), "byte-identical on a second render");
    }

    #[test]
    fn list_with_an_absent_child_is_empty() {
        let m = networks_module();
        let out = lower(&m, &node("net"));
        match find(&out, "virtualisation.incus.preseed.networks") {
            Some(NixExpr::List(items)) => assert!(items.is_empty()),
            other => panic!("networks = {other:?}"),
        }
    }

    #[test]
    fn list_opt_drops_the_assignment_when_empty_and_keeps_it_otherwise() {
        let manifest = "module name=\"lo\" version=\"0.1.0\" {\n    claims-node \"lo\"\n    schema {\n        child \"r\" repeated=#true {\n            prop \"from\" type=\"int\" required=#true\n        }\n    }\n    emit {\n        list-opt \"a.b\" from \"r\" {\n            set \"from\" (scalar)\"{r.from}\"\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("lo")).expect("loads");
        assert!(find(&lower(&m, &node("lo")), "a.b").is_none());
        match find(&lower(&m, &node("lo {\n    r from=1\n}")), "a.b") {
            Some(NixExpr::List(items)) => assert_eq!(items.len(), 1),
            other => panic!("a.b = {other:?}"),
        }
    }

    #[test]
    fn list_wraps_interpolated_values_in_a_list() {
        let manifest = "module name=\"li\" version=\"0.1.0\" {\n    claims-node \"li\"\n    schema {\n        child \"share\" repeated=#true {\n            arg \"tag\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        for-each \"s\" in \"share\" {\n            set \"x.{s.tag}.lowerdir\" (list)\"/mnt/shares/{s.tag}\"\n            set \"x.{s.tag}.options\" (list)\"ro\" \"noatime\" 3 #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("li")).expect("loads");
        let out = lower(&m, &node("li {\n    share \"src\"\n}"));
        match find(&out, "x.\"src\".lowerdir") {
            Some(NixExpr::List(items)) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], NixExpr::Str(s) if s == "/mnt/shares/src"));
            }
            other => panic!("lowerdir = {other:?}"),
        }
        match find(&out, "x.\"src\".options") {
            Some(NixExpr::List(items)) => {
                assert_eq!(items.len(), 4);
                assert!(matches!(&items[2], NixExpr::Int(3)));
                assert!(matches!(&items[3], NixExpr::Bool(true)));
            }
            other => panic!("options = {other:?}"),
        }
    }

    #[test]
    fn list_with_an_unknown_lookup_fails_to_load() {
        let manifest = "module name=\"lb\" version=\"0.1.0\" {\n    claims-node \"lb\"\n    schema {\n    }\n    emit {\n        set \"a.b\" (list)\"{nope}\"\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        assert!(DeclarativeModule::from_kdl(&doc, std::path::Path::new("lb")).is_err());
    }

    #[test]
    fn collect_opt_emits_the_list_when_present() {
        let manifest = "module name=\"co\" version=\"0.1.0\" {\n    claims-node \"co\"\n    schema {\n        child \"item\" type=\"string\" repeated=#true\n    }\n    emit {\n        set \"a.b\" (collect-opt)\"item\"\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("co")).expect("loads");
        let out = lower(&m, &node("co {\n    item \"x\"\n    item \"y\"\n}"));
        match find(&out, "a.b") {
            Some(NixExpr::List(items)) => assert_eq!(items.len(), 2),
            other => panic!("a.b = {other:?}"),
        }
    }

    #[test]
    fn collect_opt_emits_nothing_when_empty() {
        let manifest = "module name=\"co\" version=\"0.1.0\" {\n    claims-node \"co\"\n    schema {\n        child \"item\" type=\"string\" repeated=#true\n    }\n    emit {\n        set \"a.b\" (collect-opt)\"item\"\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("co")).expect("loads");
        let out = lower(&m, &node("co"));
        assert!(
            find(&out, "a.b").is_none(),
            "empty collect-opt emits no assignment"
        );
    }

    #[test]
    fn collect_still_emits_empty_list_when_empty() {
        // Guard the non-breaking promise: plain (collect) is unchanged.
        let manifest = "module name=\"c\" version=\"0.1.0\" {\n    claims-node \"c\"\n    schema {\n        child \"item\" type=\"string\" repeated=#true\n    }\n    emit {\n        set \"a.b\" (collect)\"item\"\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("c")).expect("loads");
        let out = lower(&m, &node("c"));
        match find(&out, "a.b") {
            Some(NixExpr::List(items)) => assert!(items.is_empty()),
            other => panic!("a.b = {other:?}"),
        }
    }

    #[test]
    fn dry_check_rejects_collect_opt_over_a_non_repeated_child() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        child \"item\" type=\"string\"\n    }\n    emit {\n        set \"a.b\" (collect-opt)\"item\"\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("not a repeated child"),
            "got: {err}"
        );
    }

    #[test]
    fn list_when_flag_drops_an_inner_attr() {
        let manifest = "module name=\"net\" version=\"0.1.0\" {\n    claims-node \"net\"\n    schema {\n        child \"network\" repeated=#true {\n            prop \"name\" type=\"string\" required=#true\n            prop \"managed\" type=\"bool\"\n        }\n    }\n    emit {\n        list \"a.networks\" from \"network\" {\n            set \"name\" \"{network.name}\"\n            when-flag \"network.managed\" {\n                set \"managed\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("net")).expect("loads");
        let out = lower(
            &m,
            &node("net {\n    network name=\"on\" managed=#true\n    network name=\"off\"\n}"),
        );
        match find(&out, "a.networks") {
            Some(NixExpr::List(items)) => {
                let has = |i: usize, k: &str| matches!(&items[i], NixExpr::AttrSet(m) if m.contains_key(&AttrKey::Ident(k.into())));
                assert!(has(0, "managed"), "managed=true keeps the attr");
                assert!(!has(1, "managed"), "managed absent drops the attr");
            }
            other => panic!("networks = {other:?}"),
        }
    }

    #[test]
    fn dry_check_rejects_list_over_a_non_repeated_child() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        child \"net\" type=\"string\"\n    }\n    emit {\n        list \"a.b\" from \"net\" {\n            set \"name\" \"{net}\"\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("not a repeated child"),
            "got: {err}"
        );
    }

    #[test]
    fn list_rejects_a_duplicate_inner_attr_path() {
        let manifest = "module name=\"dup\" version=\"0.1.0\" {\n    claims-node \"dup\"\n    schema {\n        child \"x\" repeated=#true {\n            prop \"a\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        list \"p.q\" from \"x\" {\n            set \"name\" \"{x.a}\"\n            set \"name\" \"{x.a}\"\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("dup"))
            .expect("loads (dup is a generate-time error)");
        let mut diags = Vec::new();
        let mut ctx = crate::LowerCtx::new(crate::Scope { host: "h".into() }, &mut diags, vec![]);
        let err = m
            .lower(&node("dup {\n    x a=\"1\"\n}"), &mut ctx)
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("duplicate")
                || format!("{err}").contains("both a value and a set"),
            "got: {err}"
        );
    }

    #[test]
    fn a_missing_required_bool_subfield_still_errors() {
        // An optional bool subfield defaults to false, but a required one left absent must
        // still error at generate (no silent fallback).
        let manifest = "module name=\"req\" version=\"0.1.0\" {\n    claims-node \"req\"\n    schema {\n        child \"x\" repeated=#true {\n            prop \"name\" type=\"string\" required=#true\n            prop \"on\" type=\"bool\" required=#true\n        }\n    }\n    emit {\n        list \"p.q\" from \"x\" {\n            set \"name\" \"{x.name}\"\n            when-flag \"x.on\" {\n                set \"on\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("req")).expect("loads");
        let mut diags = Vec::new();
        let mut ctx = crate::LowerCtx::new(crate::Scope { host: "h".into() }, &mut diags, vec![]);
        // `on` is required but omitted on this instance: its `when-flag` lookup must error.
        let err = m
            .lower(&node("req {\n    x name=\"a\"\n}"), &mut ctx)
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("on"),
            "expected a missing-field error for `on`: {err}"
        );
    }

    fn lower_with_backend(
        src: &str,
        node_src: &str,
        backend: crate::SecretsBackend,
    ) -> LowerOutput {
        let doc = src.parse::<kdl::KdlDocument>().expect("parse module");
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("t")).expect("from_kdl");
        let n = node_src
            .parse::<kdl::KdlDocument>()
            .unwrap()
            .nodes()
            .first()
            .unwrap()
            .clone();
        let mut diags = Vec::new();
        let mut ctx = crate::LowerCtx::new(crate::Scope { host: "h".into() }, &mut diags, vec![])
            .with_secrets_backend(backend);
        module.lower(&n, &mut ctx).expect("lower")
    }

    const SECRET_MODULE: &str = "module name=\"t\" version=\"1.0.0\" {\n\
        \x20   claims-node \"t\"\n\
        \x20   schema {\n\
        \x20       child \"key\" repeated=#true {\n\
        \x20           prop \"secret\" type=\"string\" required=#true\n\
        \x20       }\n\
        \x20   }\n\
        \x20   emit {\n\
        \x20       for-each \"k\" in \"key\" {\n\
        \x20           set \"services.x.file\" (secret)\"{k.secret}\"\n\
        \x20       }\n\
        \x20   }\n\
        }";

    fn raw_src(out: &LowerOutput) -> String {
        match &out.units[0].assignment.value {
            NixExpr::Raw(r) => r.src.clone(),
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn secret_sops_backend_emits_sops_path() {
        let out = lower_with_backend(
            SECRET_MODULE,
            "t {\n    key secret=\"tailscale-authkey\"\n}",
            crate::SecretsBackend::SopsNix,
        );
        assert_eq!(
            raw_src(&out),
            "config.sops.secrets.\"tailscale-authkey\".path"
        );
    }

    #[test]
    fn secret_agenix_backend_emits_age_path() {
        let out = lower_with_backend(
            SECRET_MODULE,
            "t {\n    key secret=\"tailscale-authkey\"\n}",
            crate::SecretsBackend::Agenix,
        );
        assert_eq!(
            raw_src(&out),
            "config.age.secrets.\"tailscale-authkey\".path"
        );
    }

    #[test]
    fn secret_name_is_escaped() {
        let out = lower_with_backend(
            SECRET_MODULE,
            "t {\n    key secret=\"a\\\"b\"\n}",
            crate::SecretsBackend::SopsNix,
        );
        // The embedded quote must be backslash-escaped so it cannot break out of the literal.
        assert_eq!(raw_src(&out), "config.sops.secrets.\"a\\\"b\".path");
    }

    #[test]
    fn secret_name_dollar_brace_is_escaped() {
        let out = lower_with_backend(
            SECRET_MODULE,
            "t {\n    key secret=\"a${x}b\"\n}",
            crate::SecretsBackend::SopsNix,
        );
        // `${` is Nix antiquotation even inside a quoted attr name, so it must be
        // backslash-escaped, not passed through live.
        assert_eq!(raw_src(&out), "config.sops.secrets.\"a\\${x}b\".path");
    }

    #[test]
    fn secret_non_scalar_name_fails_dry_check() {
        // `{key}` (the whole repeated child, a list/scope) is not a scalar, so the module must
        // fail to load.
        let src = "module name=\"t\" version=\"1.0.0\" {\n\
            \x20   claims-node \"t\"\n\
            \x20   schema { child \"key\" repeated=#true { prop \"secret\" type=\"string\" required=#true } }\n\
            \x20   emit { set \"services.x.file\" (secret)\"{key}\" }\n\
            }";
        let doc = src.parse::<kdl::KdlDocument>().unwrap();
        assert!(DeclarativeModule::from_kdl(&doc, std::path::Path::new("t")).is_err());
    }
}
