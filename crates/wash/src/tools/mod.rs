pub mod build;
pub mod edit;
pub mod gh_pr;
pub mod git_state;
pub mod read;
pub mod search;
pub mod test_run;

use crate::mcp::Tool;

pub fn all() -> Vec<Tool> {
    vec![
        search::tool(),
        read::tool(),
        edit::tool(),
        git_state::tool(),
        test_run::tool(),
        build::tool(),
        gh_pr::tool(),
    ]
}
