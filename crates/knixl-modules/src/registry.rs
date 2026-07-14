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
}

impl Default for Registry { fn default() -> Self { Self::new() } }
