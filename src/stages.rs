//! Spill-over order and the rule for when a runner runs.
//!
//! Runners are ordered by number, then by host: A-1, B-1, C-1, A-2, B-2, ...
//! A runner runs when every runner before it is busy or down. The first one
//! always runs, so the fleet spills over one runner at a time.

use crate::config::{Host, runner_name};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Running a job.
    Busy,
    /// Running and waiting for a job.
    Idle,
    /// Not running, ready to start.
    Stopped,
    /// Cannot run, or its host did not answer.
    Down,
}

/// What the agent does with one of its own runners.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Leave,
}

pub fn order(hosts: &[Host]) -> Vec<String> {
    let most = hosts.iter().map(|h| h.runners).max().unwrap_or(0);
    (1..=most)
        .flat_map(|n| {
            hosts
                .iter()
                .filter(move |h| n <= h.runners)
                .map(move |h| runner_name(&h.name, n))
        })
        .collect()
}

/// A runner missing from `states` counts as down.
pub fn wanted(order: &[String], states: &HashMap<String, State>, runner: &str) -> bool {
    order
        .iter()
        .take_while(|r| r.as_str() != runner)
        .all(|r| matches!(states.get(r), Some(State::Busy | State::Down) | None))
}

pub fn action(wanted: bool, state: State) -> Action {
    match (wanted, state) {
        (true, State::Stopped) => Action::Start,
        // A busy runner finishes its job first.
        (false, State::Idle) => Action::Stop,
        _ => Action::Leave,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Kind;
    use State::*;

    fn host(name: &str, runners: u32) -> Host {
        Host {
            name: name.into(),
            endpoint: "http://x".into(),
            runners,
            listen: None,
            kind: Kind::Docker,
            root: None,
        }
    }

    fn states(list: &[(&str, State)]) -> HashMap<String, State> {
        list.iter().map(|(r, s)| (r.to_string(), *s)).collect()
    }

    #[test]
    fn orders_by_number_then_host() {
        let o = order(&[host("A", 2), host("B", 1), host("C", 3)]);
        assert_eq!(
            o,
            [
                "runner-A-1",
                "runner-B-1",
                "runner-C-1",
                "runner-A-2",
                "runner-C-2",
                "runner-C-3"
            ]
        );
    }

    #[test]
    fn the_first_runner_always_runs() {
        let o = order(&[host("A", 1), host("B", 1)]);
        assert!(wanted(&o, &HashMap::new(), "runner-A-1"));
    }

    #[test]
    fn a_runner_runs_only_when_all_before_it_are_busy_or_down() {
        let o = order(&[host("A", 1), host("B", 1), host("C", 1)]);
        let s = states(&[("runner-A-1", Busy), ("runner-B-1", Idle)]);
        assert!(wanted(&o, &s, "runner-B-1"));
        assert!(!wanted(&o, &s, "runner-C-1"));
        let s = states(&[("runner-A-1", Busy), ("runner-B-1", Down)]);
        assert!(wanted(&o, &s, "runner-C-1"));
        // B's host did not answer: its runner is missing, so counted as down.
        let s = states(&[("runner-A-1", Busy)]);
        assert!(wanted(&o, &s, "runner-C-1"));
        let s = states(&[("runner-A-1", Stopped), ("runner-B-1", Busy)]);
        assert!(!wanted(&o, &s, "runner-C-1"));
    }

    #[test]
    fn starts_when_wanted_and_stops_only_idle_runners() {
        assert_eq!(action(true, Stopped), Action::Start);
        assert_eq!(action(true, Idle), Action::Leave);
        assert_eq!(action(true, Down), Action::Leave);
        assert_eq!(action(false, Idle), Action::Stop);
        assert_eq!(action(false, Busy), Action::Leave);
        assert_eq!(action(false, Stopped), Action::Leave);
    }
}
