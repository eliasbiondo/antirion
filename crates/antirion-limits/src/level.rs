//! The policy levels that declare limits and budgets.

/// Where a limit or budget is declared, broadest first: the order breaks ties between refusals.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Level {
    Gateway,
    Organization,
    Team,
    Project,
    Key,
    Route,
    Model,
    /// A route-tree node; its owner is the node's route path.
    Node,
    Deployment,
}

impl Level {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Gateway => "gateway",
            Level::Organization => "organization",
            Level::Team => "team",
            Level::Project => "project",
            Level::Key => "key",
            Level::Route => "route",
            Level::Model => "model",
            Level::Node => "node",
            Level::Deployment => "deployment",
        }
    }

    /// The level named `s` in the Admin API's paths.
    #[must_use]
    pub fn parse(s: &str) -> Option<Level> {
        [
            Level::Gateway,
            Level::Organization,
            Level::Team,
            Level::Project,
            Level::Key,
            Level::Route,
            Level::Model,
            Level::Node,
            Level::Deployment,
        ]
        .into_iter()
        .find(|l| l.as_str() == s)
    }

    /// The config collection holding the declaring resources; empty for the gateway and nodes.
    #[must_use]
    pub const fn collection(self) -> &'static str {
        match self {
            Level::Gateway | Level::Node => "",
            Level::Organization => "organizations",
            Level::Team => "teams",
            Level::Project => "projects",
            Level::Key => "keys",
            Level::Route => "routes",
            Level::Model => "models",
            Level::Deployment => "deployments",
        }
    }

    /// Whether the counters of this level belong to one tenant (organization) only.
    #[must_use]
    pub const fn is_tenant(self) -> bool {
        matches!(
            self,
            Level::Organization | Level::Team | Level::Project | Level::Key
        )
    }
}
