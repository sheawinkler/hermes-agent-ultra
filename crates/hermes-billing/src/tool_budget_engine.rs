use thiserror::Error;

use crate::tool_budget::{ToolBudget, ToolId};

#[derive(Debug, Error)]
pub enum BudgetError {
    #[error("tool budget exceeded")]
    Exceeded,
}

pub struct ToolBudgetEngine {
    budgets: std::collections::HashMap<ToolId, ToolBudget>,
}

impl ToolBudgetEngine {
    pub fn new(budgets: std::collections::HashMap<ToolId, ToolBudget>) -> Self {
        Self { budgets }
    }

    pub fn check_and_deduct(&mut self, tool: ToolId, amount: u32) -> Result<(), BudgetError> {
        let budget = self.budgets.get_mut(&tool).ok_or(BudgetError::Exceeded)?;
        if budget.used + amount > budget.monthly_limit {
            return Err(BudgetError::Exceeded);
        }
        budget.used += amount;
        Ok(())
    }
}
