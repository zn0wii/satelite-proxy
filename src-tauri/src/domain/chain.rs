use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How a [`NodePool`] selects its member nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PoolMode {
    /// Fixed set of stable node ids. Never changes on subscription refresh.
    Explicit { node_ids: Vec<String> },
    /// Dynamic keyword filter, re-evaluated against the current node list
    /// every time a config is built (same semantics as `Rule::smart_include`
    /// / `smart_exclude`).
    Keyword {
        #[serde(default)]
        include: Vec<String>,
        #[serde(default)]
        exclude: Vec<String>,
    },
}

/// Named, reusable node pool. Referenced by [`ProxyChain`] hops and (later)
/// by `Rule`/`RuleSet` as a routing target, so multiple rules can share one
/// pool definition instead of repeating keyword filters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodePool {
    pub id: String,
    pub name: String,
    pub mode: PoolMode,
}

impl NodePool {
    pub fn new(name: &str, mode: PoolMode) -> Self {
        let id = Self::generate_id(name);
        Self {
            id,
            name: name.trim().to_string(),
            mode,
        }
    }

    /// `pool-<hash>` — disjoint from node ids (`ProxyNode::compute_id` is bare
    /// hex) and from chain ids (`chain-` prefix), so tag spaces never collide.
    fn generate_id(name: &str) -> String {
        let mut h = Sha256::new();
        h.update(name.as_bytes());
        h.update(b"|");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        h.update(nanos.to_le_bytes());
        // Extra entropy so rapid creates don't collide (mirrors RuleSet::new_user).
        h.update(std::process::id().to_le_bytes());
        format!("pool-{}", hex::encode(&h.finalize()[..10]))
    }

    /// Selector/urltest outbound tag for this pool (stable, short).
    pub fn outbound_tag(&self) -> String {
        pool_outbound_tag_for_id(&self.id)
    }
}

/// Selector/urltest outbound tag for a pool id, without needing the full
/// [`NodePool`] — used where only the id is on hand (e.g. resolving a
/// [`ChainHop::Pool`] against a live-pool-id set). Must stay in sync with
/// [`NodePool::outbound_tag`], which just forwards here.
pub fn pool_outbound_tag_for_id(id: &str) -> String {
    format!("pool-{}", &id[id.len().saturating_sub(20)..])
}

/// One hop in a [`ProxyChain`] — either a single pinned node or a pool
/// (resolved to that pool's selector/urltest outbound at build time).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChainHop {
    Node { node_id: String },
    Pool { pool_id: String },
}

/// Named, ordered chain of hops. sing-box has no native "chain" object —
/// this is built into a `detour` chain: rules route to the LAST hop's
/// outbound tag (the exit the internet sees), and each hop[i ≥ 1] sets
/// `detour` to hop[i-1]'s tag, so traffic flows
/// client → hop[0] → hop[1] → … → hop[N-1] → internet. Rules reference the
/// chain by id and route to `chain_outbound_tag()` (the exit hop's tag).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyChain {
    pub id: String,
    pub name: String,
    /// Ordered hops, entry point first. Must have at least 2 hops — a
    /// single-hop chain is just a node/pool pin and should use `RuleTarget::Node`
    /// / `RuleTarget::Pool` directly instead.
    pub hops: Vec<ChainHop>,
}

impl ProxyChain {
    pub fn new(name: &str, hops: Vec<ChainHop>) -> Self {
        let id = Self::generate_id(name);
        Self {
            id,
            name: name.trim().to_string(),
            hops,
        }
    }

    fn generate_id(name: &str) -> String {
        let mut h = Sha256::new();
        h.update(name.as_bytes());
        h.update(b"|");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        h.update(nanos.to_le_bytes());
        h.update(std::process::id().to_le_bytes());
        format!("chain-{}", hex::encode(&h.finalize()[..10]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_ids_are_disjoint_and_stable_prefix() {
        let pool = NodePool::new(
            "香港节点",
            PoolMode::Keyword {
                include: vec!["香港".into()],
                exclude: vec![],
            },
        );
        assert!(pool.id.starts_with("pool-"));
        assert!(pool.outbound_tag().starts_with("pool-"));
    }

    #[test]
    fn chain_ids_are_disjoint_from_pool_ids() {
        let chain = ProxyChain::new(
            "落地链",
            vec![
                ChainHop::Pool {
                    pool_id: "pool-abc".into(),
                },
                ChainHop::Node {
                    node_id: "node-xyz".into(),
                },
            ],
        );
        assert!(chain.id.starts_with("chain-"));
        assert_ne!(
            chain.id,
            NodePool::new("x", PoolMode::Explicit { node_ids: vec![] }).id
        );
    }

    #[test]
    fn rapid_creates_do_not_collide() {
        let a = NodePool::new("同名", PoolMode::Explicit { node_ids: vec![] });
        let b = NodePool::new("同名", PoolMode::Explicit { node_ids: vec![] });
        assert_ne!(a.id, b.id);
    }
}
