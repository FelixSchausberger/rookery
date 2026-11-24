// The board as rookery-core reports it. The TUI renders; the DTOs, the null
// rules and the five derived lanes live in the core so the CLI and the board
// cannot disagree about a workspace.
#[allow(unused_imports)]
pub use rookery_core::board::{
    Board, CleanInfo as Clean, Hold, Lane, LaneView, SessionInfo as Session, TaskView,
    WaitInfo as Wait, WorkspaceRow as Workspace,
};
#[allow(unused_imports)]
pub use rookery_core::jj::State;
#[allow(unused_imports)]
pub use rookery_core::task::Task;
