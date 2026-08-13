//! CP-authoritative delegation policy.
//!
//! Every check here operates exclusively on CP-owned data: authenticated
//! identity claims, the CP-constructed ancestry chain, and CP config. Facade
//! checks in the runtime are defense-in-depth only; nothing in this module
//! trusts a client-supplied policy input.

use chrono::{DateTime, Utc};

use crate::config::NamespacePolicy;
use crate::proto::AgentType;

pub struct PolicyInput<'a> {
    /// Authenticated initiator identity.
    pub from_namespace: &'a str,
    pub from_name: &'a str,
    pub from_type: &'a AgentType,
    /// Target namespace (v1: always the initiator's namespace; the router
    /// resolves selectors within it. Cross-namespace grants are future work).
    pub target_namespace: &'a str,
    /// Logical name of the resolved target.
    pub target_name: &'a str,
    /// CP-constructed chain for the *parent* delegation (empty for a root
    /// delegation). Elements are `namespace/name`.
    pub parent_chain: &'a [String],
    pub deadline: DateTime<Utc>,
    pub parent_deadline: Option<DateTime<Utc>>,
    pub now: DateTime<Utc>,
    pub max_deadline_secs: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PolicyDenial {
    WorkerInitiation,
    DepthExceeded { max: u32, would_be: u32 },
    Cycle { target: String },
    CrossNamespace,
    DeadlinePast,
    DeadlineTooLong { max_secs: u64 },
    DeadlineExceedsParent,
}

impl std::fmt::Display for PolicyDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyDenial::WorkerInitiation => {
                write!(f, "workers may not initiate delegations in this namespace")
            }
            PolicyDenial::DepthExceeded { max, would_be } => write!(
                f,
                "delegation depth {would_be} exceeds namespace max_depth {max}"
            ),
            PolicyDenial::Cycle { target } => {
                write!(f, "cycle: {target} is already in the delegation chain")
            }
            PolicyDenial::CrossNamespace => {
                write!(f, "cross-namespace delegation is not granted")
            }
            PolicyDenial::DeadlinePast => write!(f, "deadline is in the past"),
            PolicyDenial::DeadlineTooLong { max_secs } => {
                write!(f, "deadline exceeds the CP cap of {max_secs}s")
            }
            PolicyDenial::DeadlineExceedsParent => {
                write!(f, "child deadline exceeds the parent's remaining budget")
            }
        }
    }
}

/// Evaluate the full CP-side policy for one delegation attempt.
pub fn check(input: &PolicyInput<'_>, ns_policy: &NamespacePolicy) -> Result<(), PolicyDenial> {
    // 1. Initiator role.
    if *input.from_type == AgentType::Worker && !ns_policy.allow_worker_initiation {
        return Err(PolicyDenial::WorkerInitiation);
    }

    // 2. Namespace boundary (v1: strict).
    if input.from_namespace != input.target_namespace {
        return Err(PolicyDenial::CrossNamespace);
    }

    // 3. Depth: the new chain would be parent_chain + initiator; its length
    //    equals the delegation depth (root delegation → depth 1).
    let would_be = input.parent_chain.len() as u32 + 1;
    if would_be > ns_policy.max_depth {
        return Err(PolicyDenial::DepthExceeded {
            max: ns_policy.max_depth,
            would_be,
        });
    }

    // 4. Cycle: target must not already be an ancestor (or the initiator).
    let target_id = format!("{}/{}", input.target_namespace, input.target_name);
    let from_id = format!("{}/{}", input.from_namespace, input.from_name);
    if target_id == from_id || input.parent_chain.contains(&target_id) {
        return Err(PolicyDenial::Cycle { target: target_id });
    }

    // 5. Deadline sanity and caps.
    if input.deadline <= input.now {
        return Err(PolicyDenial::DeadlinePast);
    }
    let remaining = (input.deadline - input.now).num_seconds();
    if remaining > input.max_deadline_secs as i64 {
        return Err(PolicyDenial::DeadlineTooLong {
            max_secs: input.max_deadline_secs,
        });
    }
    if let Some(parent) = input.parent_deadline {
        if input.deadline > parent {
            return Err(PolicyDenial::DeadlineExceedsParent);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn base<'a>(now: DateTime<Utc>, chain: &'a [String]) -> PolicyInput<'a> {
        PolicyInput {
            from_namespace: "prod",
            from_name: "koudu",
            from_type: &AgentType::Primary,
            target_namespace: "prod",
            target_name: "worker-1",
            parent_chain: chain,
            deadline: now + Duration::seconds(60),
            parent_deadline: None,
            now,
            max_deadline_secs: 1800,
        }
    }

    fn default_policy() -> NamespacePolicy {
        NamespacePolicy::default()
    }

    #[test]
    fn root_primary_delegation_passes() {
        let now = Utc::now();
        assert!(check(&base(now, &[]), &default_policy()).is_ok());
    }

    #[test]
    fn worker_initiation_denied_by_default_allowed_by_config() {
        let now = Utc::now();
        let chain: Vec<String> = vec![];
        let mut input = base(now, &chain);
        input.from_type = &AgentType::Worker;
        assert_eq!(
            check(&input, &default_policy()),
            Err(PolicyDenial::WorkerInitiation)
        );
        let relaxed = NamespacePolicy {
            max_depth: 2,
            allow_worker_initiation: true,
        };
        assert!(check(&input, &relaxed).is_ok());
    }

    #[test]
    fn depth_exceeded_at_default_depth_one() {
        let now = Utc::now();
        let chain = vec!["prod/root".to_string()];
        let input = base(now, &chain);
        assert_eq!(
            check(&input, &default_policy()),
            Err(PolicyDenial::DepthExceeded {
                max: 1,
                would_be: 2
            })
        );
        let relaxed = NamespacePolicy {
            max_depth: 2,
            allow_worker_initiation: true,
        };
        assert!(check(&input, &relaxed).is_ok());
    }

    #[test]
    fn cycle_rejected_including_self() {
        let now = Utc::now();
        let chain = vec!["prod/worker-1".to_string()];
        let relaxed = NamespacePolicy {
            max_depth: 5,
            allow_worker_initiation: true,
        };
        let input = base(now, &chain);
        assert!(matches!(
            check(&input, &relaxed),
            Err(PolicyDenial::Cycle { .. })
        ));
        // self-delegation
        let chain2: Vec<String> = vec![];
        let mut input2 = base(now, &chain2);
        input2.target_name = "koudu";
        assert!(matches!(
            check(&input2, &relaxed),
            Err(PolicyDenial::Cycle { .. })
        ));
    }

    #[test]
    fn cross_namespace_denied() {
        let now = Utc::now();
        let chain: Vec<String> = vec![];
        let mut input = base(now, &chain);
        input.target_namespace = "dev";
        assert_eq!(
            check(&input, &default_policy()),
            Err(PolicyDenial::CrossNamespace)
        );
    }

    #[test]
    fn deadline_rules() {
        let now = Utc::now();
        let chain: Vec<String> = vec![];

        let mut past = base(now, &chain);
        past.deadline = now - Duration::seconds(1);
        assert_eq!(
            check(&past, &default_policy()),
            Err(PolicyDenial::DeadlinePast)
        );

        let mut long = base(now, &chain);
        long.deadline = now + Duration::seconds(3600);
        assert_eq!(
            check(&long, &default_policy()),
            Err(PolicyDenial::DeadlineTooLong { max_secs: 1800 })
        );

        let mut over_parent = base(now, &chain);
        over_parent.deadline = now + Duration::seconds(120);
        over_parent.parent_deadline = Some(now + Duration::seconds(60));
        assert_eq!(
            check(&over_parent, &default_policy()),
            Err(PolicyDenial::DeadlineExceedsParent)
        );
    }
}
