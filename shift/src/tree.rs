use anyhow::Result;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use tracing;

/// Process tree hierarchy tracking
pub struct ProcessTree {
    tree: HashMap<u32, ProcessNode>,
    parent_map: HashMap<u32, u32>,
    children_map: HashMap<u32, Vec<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessNode {
    pub pid: u32,
    pub app_id: String,
    pub parent_pid: Option<u32>,
    pub children: Vec<u32>,
    pub depth: u32,
}

impl ProcessTree {
    pub fn new() -> Self {
        ProcessTree {
            tree: HashMap::new(),
            parent_map: HashMap::new(),
            children_map: HashMap::new(),
        }
    }

    pub fn add_process(&mut self, pid: u32, parent_pid: Option<u32>, app_id: &str) -> Result<()> {
        let depth = if let Some(ppid) = parent_pid {
            let parent_depth = self
                .tree
                .get(&ppid)
                .map(|n| n.depth)
                .ok_or_else(|| anyhow::anyhow!("Parent process {} not found", ppid))?;
            parent_depth + 1
        } else {
            0
        };

        let node = ProcessNode {
            pid,
            app_id: app_id.to_string(),
            parent_pid,
            children: Vec::new(),
            depth,
        };

        // Add to tree
        self.tree.insert(pid, node);

        // Update parent map
        if let Some(ppid) = parent_pid {
            self.parent_map.insert(pid, ppid);

            // Update children map
            self.children_map.entry(ppid).or_insert_with(Vec::new).push(pid);

            // Update parent node's children list
            if let Some(parent) = self.tree.get_mut(&ppid) {
                parent.children.push(pid);
            }
        }

        tracing::trace!(
            "Added process to tree: pid={}, parent_pid={:?}, app_id={}",
            pid,
            parent_pid,
            app_id
        );
        Ok(())
    }

    pub fn remove_process(&mut self, pid: u32) -> Result<()> {
        if let Some(node) = self.tree.remove(&pid) {
            // Remove from parent's children
            if let Some(ppid) = node.parent_pid {
                if let Some(children) = self.children_map.get_mut(&ppid) {
                    children.retain(|&c| c != pid);
                }
                if let Some(parent) = self.tree.get_mut(&ppid) {
                    parent.children.retain(|&c| c != pid);
                }
            }

            // Remove from parent map
            self.parent_map.remove(&pid);

            // Remove children map entry
            self.children_map.remove(&pid);

            tracing::trace!("Removed process from tree: pid={}", pid);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Process {} not in tree", pid))
        }
    }

    pub fn get_children(&self, pid: u32) -> Vec<u32> {
        self.children_map
            .get(&pid)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    pub fn get_parent(&self, pid: u32) -> Option<u32> {
        self.parent_map.get(&pid).copied()
    }

    pub fn get_ancestors(&self, pid: u32) -> Vec<u32> {
        let mut ancestors = Vec::new();
        let mut current_pid = pid;

        while let Some(ppid) = self.parent_map.get(&current_pid) {
            ancestors.push(*ppid);
            current_pid = *ppid;
        }

        ancestors
    }

    pub fn get_descendants(&self, pid: u32) -> Vec<u32> {
        let mut descendants = Vec::new();
        let mut queue = vec![pid];

        while let Some(current) = queue.pop() {
            if let Some(children) = self.children_map.get(&current) {
                for &child in children {
                    descendants.push(child);
                    queue.push(child);
                }
            }
        }

        descendants
    }

    pub fn get_tree_structure(&self) -> Vec<ProcessNode> {
        self.tree.values().cloned().collect()
    }

    pub fn get_process_node(&self, pid: u32) -> Option<ProcessNode> {
        self.tree.get(&pid).cloned()
    }

    pub fn get_root_processes(&self) -> Vec<u32> {
        self.tree
            .iter()
            .filter(|(_, node)| node.parent_pid.is_none())
            .map(|(pid, _)| *pid)
            .collect()
    }

    pub fn print_tree(&self) {
        for root_pid in self.get_root_processes() {
            self.print_subtree(root_pid, 0);
        }
    }

    fn print_subtree(&self, pid: u32, depth: u32) {
        if let Some(node) = self.tree.get(&pid) {
            let indent = "  ".repeat(depth as usize);
            println!("{}{} ({})", indent, node.app_id, pid);

            for &child_pid in &node.children {
                self.print_subtree(child_pid, depth + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_tree_add() {
        let mut tree = ProcessTree::new();
        tree.add_process(1, None, "init").unwrap();
        tree.add_process(2, Some(1), "shell").unwrap();

        assert_eq!(tree.get_parent(2), Some(1));
        assert_eq!(tree.get_children(1), vec![2]);
    }

    #[test]
    fn test_process_tree_ancestors() {
        let mut tree = ProcessTree::new();
        tree.add_process(1, None, "init").unwrap();
        tree.add_process(2, Some(1), "shell").unwrap();
        tree.add_process(3, Some(2), "app").unwrap();

        let ancestors = tree.get_ancestors(3);
        assert_eq!(ancestors, vec![2, 1]);
    }

    #[test]
    fn test_process_tree_descendants() {
        let mut tree = ProcessTree::new();
        tree.add_process(1, None, "init").unwrap();
        tree.add_process(2, Some(1), "shell").unwrap();
        tree.add_process(3, Some(1), "app").unwrap();
        tree.add_process(4, Some(2), "subprocess").unwrap();

        let descendants = tree.get_descendants(1);
        assert_eq!(descendants.len(), 3);
    }

    #[test]
    fn test_process_tree_removal() {
        let mut tree = ProcessTree::new();
        tree.add_process(1, None, "init").unwrap();
        tree.add_process(2, Some(1), "shell").unwrap();

        tree.remove_process(2).unwrap();
        assert_eq!(tree.get_children(1), Vec::<u32>::new());
    }
}
