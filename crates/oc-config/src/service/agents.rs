//! Agent selection and permission evaluation.

use serde_json::Value;

use crate::ConfigService;
use crate::agents::AgentState;
use crate::permissions::{Effect, PermissionRule, evaluate_rules};

impl ConfigService {
    /// Built-in and configured agents as wire `Agent.Info` objects, with the
    /// selected default agent first.
    #[must_use]
    pub fn agents(&self) -> Vec<Value> {
        let mut values = Vec::with_capacity(self.agents.len());
        if let Some(agent) = self
            .agents
            .iter()
            .find(|agent| agent.id == self.default_agent)
        {
            values.push(agent.to_value());
        }
        for agent in &self.agents {
            if agent.id != self.default_agent {
                values.push(agent.to_value());
            }
        }
        values
    }

    #[must_use]
    pub fn agent(&self, id: &str) -> Option<Value> {
        self.agents
            .iter()
            .find(|agent| agent.id == id)
            .map(AgentState::to_value)
    }

    /// Document-level permission rules in load order.
    #[must_use]
    pub fn permissions(&self) -> &[PermissionRule] {
        &self.permissions
    }

    /// Evaluate an action against the agent's composed rules.
    ///
    /// Agent rules already include the document-level permissions (pushed
    /// before per-agent rules, so the agent wins). Without a known agent, only
    /// document rules apply. No match means `ask`.
    #[must_use]
    pub fn evaluate(&self, agent: Option<&str>, action: &str, resource: &str) -> Effect {
        if let Some(agent) = agent
            && let Some(state) = self.agents.iter().find(|state| state.id == agent)
        {
            return evaluate_rules(state.permissions.iter(), action, resource);
        }
        evaluate_rules(self.permissions.iter(), action, resource)
    }
}
