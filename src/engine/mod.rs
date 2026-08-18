//! The state machine. Every write to a task goes through here, because the CLI
//! and the supervisor are both writers and consistency comes from shared code
//! rather than from a transport.
//!
//! The module is a façade over six parts:
//!
//! - [`flow`] — the core: start a step, record its result, decide what follows.
//! - [`lifecycle`] — the CLI's verbs: create, park, retry, re-run, cancel.
//! - [`checks`] — verdicts about commits, from scripts, agents and people.
//! - [`binding`] — tying a Herdr pane (and worktree) to a task.
//! - [`recovery`] — re-queueing steps orphaned by a dead supervisor.
//! - [`resolve`] — redeeming deferred steps from Herdr's raw events.
//!
//! Everything is re-exported flat, so callers say `engine::finish_step` and
//! never care where it lives.

mod binding;
mod checks;
mod flow;
mod lifecycle;
mod plan;
mod policy;
mod recovery;
pub mod resolve;
mod step;
mod transition;

pub use binding::{Binding, bind_pane};
pub use checks::{Submission, settle_by_human, submit_check};
pub use flow::{Started, begin_step, finish_step};
pub use lifecycle::{cancel, create_task, park_task, retry, run_pipeline};
pub use plan::Plan;
pub use policy::{awaits_human, policy_for};
pub use recovery::recover_orphans;
pub use resolve::{AgentStatus, Drained, drain};
pub use step::{StepAt, StepReport, StepSpec, environment, run as run_step};
pub use transition::{
    Applied, Decision, Outcome as TransitionOutcome, transition, transition_with,
};
