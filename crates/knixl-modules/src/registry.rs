use std::collections::BTreeMap;
use crate::Module;

pub struct Registry { by_node: BTreeMap<String, Box<dyn Module>> }

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("two modules claim node '{0}'")]
    Duplicate(String),
}

impl Registry {
    pub fn new() -> Self { Self { by_node: BTreeMap::new() } }

    /// Two modules claiming the same node is a hard config error, not last-wins.
    pub fn register(&mut self, m: Box<dyn Module>) -> Result<(), RegistryError> {
        let name = m.node_name().to_string();
        if self.by_node.contains_key(&name) { return Err(RegistryError::Duplicate(name)); }
        self.by_node.insert(name, m);
        Ok(())
    }

    pub fn get(&self, node_name: &str) -> Option<&dyn Module> {
        self.by_node.get(node_name).map(|b| b.as_ref())
    }

    /// Every registered module, keyed by node name, in node order. For listing (e.g. the TUI
    /// browser and future `knixl doc` index).
    pub fn entries(&self) -> impl Iterator<Item = (&str, &dyn Module)> {
        self.by_node.iter().map(|(k, v)| (k.as_str(), v.as_ref()))
    }

    /// Every registered module's name and version, for the lock's `module` entries.
    pub fn module_versions(&self) -> BTreeMap<String, semver::Version> {
        self.by_node
            .values()
            .map(|m| {
                let id = m.id();
                (id.name, id.version)
            })
            .collect()
    }
}

impl Default for Registry { fn default() -> Self { Self::new() } }
