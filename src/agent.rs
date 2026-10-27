//! The loop on each host: read the states of the runners before ours, start or
//! stop ours, and serve our states to the hosts after us.

use crate::config::{Config, Host, runner_name};
use crate::local::Runners;
use crate::stages::{self, Action, State};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A runner that stops this soon after a start is crash-looping.
const QUICK_EXIT: Duration = Duration::from_secs(60);
/// How long a crash-looping runner is held as down before the next try.
const HOLD: Duration = Duration::from_secs(300);

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub host: String,
    pub runners: BTreeMap<String, State>,
}

/// What the endpoint serves, and when the loop last brought it up to date.
pub struct Published {
    pub status: Status,
    pub at: Instant,
}

pub type Shared = Arc<Mutex<Published>>;

/// Per runner: when this agent last started it, and until when it is held.
#[derive(Default)]
pub struct Memory {
    started: HashMap<String, Instant>,
    held: HashMap<String, Instant>,
}

pub fn run(config: &Config, me: &Host, mut runners: Box<dyn Runners>) -> Result<(), String> {
    // Serve real states from the first answer: an empty list reads as all down.
    let first = (1..=me.runners)
        .map(|n| runner_name(&me.name, n))
        .map(|r| {
            let s = runners.state(&r);
            (r, s)
        })
        .collect();
    let shared: Shared = Arc::new(Mutex::new(Published {
        status: Status {
            host: me.name.clone(),
            runners: first,
        },
        at: Instant::now(),
    }));
    let listen = config.listen(me);
    let server =
        tiny_http::Server::http(listen).map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    eprintln!("{}: serving status on {listen}", me.name);
    let poll = Duration::from_secs(config.poll);
    // Older than this, the loop is stuck and the states may be wrong.
    let stale = poll * 3 + Duration::from_secs(60);
    let server = {
        let shared = shared.clone();
        std::thread::spawn(move || serve(server, shared, stale))
    };
    let order = stages::order(&config.hosts);
    let mut errors = HashMap::new();
    let mut memory = Memory::default();
    loop {
        if server.is_finished() {
            // tiny_http stops accepting after one accept error; let the
            // service manager restart us rather than go silent.
            return Err("the status endpoint stopped".into());
        }
        let mut states = fetch_before(config, me, &order, &mut errors);
        let ours = tick(
            me,
            &order,
            &mut states,
            runners.as_mut(),
            &mut memory,
            Instant::now(),
        );
        let mut p = shared.lock().unwrap_or_else(|e| e.into_inner());
        p.status.runners = ours;
        p.at = Instant::now();
        drop(p);
        std::thread::sleep(poll);
    }
}

/// One pass over this host's runners, in order, so that starting runner-X-1
/// is seen when deciding about runner-X-2 in the same pass. Returns their states.
pub fn tick(
    me: &Host,
    order: &[String],
    states: &mut HashMap<String, State>,
    runners: &mut dyn Runners,
    memory: &mut Memory,
    now: Instant,
) -> BTreeMap<String, State> {
    let mut ours = BTreeMap::new();
    for n in 1..=me.runners {
        let r = runner_name(&me.name, n);
        let mut state = runners.state(&r);
        if state == State::Stopped {
            let quick = memory
                .started
                .remove(&r)
                .is_some_and(|t| now.duration_since(t) < QUICK_EXIT);
            if quick {
                eprintln!("{r}: stopped soon after it was started; holding it as down");
                memory.held.insert(r.clone(), now + HOLD);
            }
            if memory.held.get(&r).is_some_and(|until| now < *until) {
                state = State::Down;
            }
        } else {
            memory.held.remove(&r);
        }
        let wanted = stages::wanted(order, states, &r);
        let result = match stages::action(wanted, state) {
            Action::Start => runners.start(&r).map(|()| {
                memory.started.insert(r.clone(), now);
                state = State::Idle;
                "started"
            }),
            // Check again: a job may have arrived since.
            Action::Stop if runners.state(&r) != State::Idle => Ok(""),
            Action::Stop => runners.stop(&r).map(|()| {
                memory.started.remove(&r);
                state = State::Stopped;
                "stopped"
            }),
            Action::Leave => Ok(""),
        };
        match result {
            Ok("") => {}
            Ok(done) => eprintln!("{r}: {done}"),
            Err(e) => {
                eprintln!("{r}: {e}");
                state = State::Down;
            }
        }
        states.insert(r.clone(), state);
        ours.insert(r, state);
    }
    ours
}

/// The states of the runners on hosts that have one before our last runner.
/// A host that does not answer leaves its runners out, and so counts as down.
/// `errors` holds each host's last error, so a lasting one is logged once.
fn fetch_before(
    config: &Config,
    me: &Host,
    order: &[String],
    errors: &mut HashMap<String, String>,
) -> HashMap<String, State> {
    let last = runner_name(&me.name, me.runners);
    let earlier = &order[..order.iter().position(|r| *r == last).unwrap_or(0)];
    let mut states = HashMap::new();
    for h in &config.hosts {
        let prefix = format!("runner-{}-", h.name);
        if h.name == me.name || !earlier.iter().any(|r| r.starts_with(&prefix)) {
            continue;
        }
        let error = match fetch(&h.endpoint) {
            Ok(status) if status.host == h.name => {
                states.extend(own_runners(h, status));
                None
            }
            Ok(status) => Some(format!("answers as host {}", status.host)),
            Err(e) => Some(e),
        };
        match (error, errors.get(&h.name)) {
            (Some(e), Some(last)) if *last == e => {}
            (Some(e), _) => {
                eprintln!("{}: {e}", h.endpoint);
                errors.insert(h.name.clone(), e);
            }
            (None, Some(_)) => {
                eprintln!("{}: answers again", h.endpoint);
                errors.remove(&h.name);
            }
            (None, None) => {}
        }
    }
    states
}

/// Only the runners the fleet file gives that host: a peer cannot speak for others.
fn own_runners(h: &Host, status: Status) -> impl Iterator<Item = (String, State)> + '_ {
    status
        .runners
        .into_iter()
        .filter(|(r, _)| (1..=h.runners).any(|n| *r == runner_name(&h.name, n)))
}

pub fn fetch(endpoint: &str) -> Result<Status, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .into();
    let body = agent
        .get(format!("{}/status", endpoint.trim_end_matches('/')))
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

fn serve(server: tiny_http::Server, shared: Shared, stale: Duration) {
    for request in server.incoming_requests() {
        let response = if request.url() != "/status" {
            tiny_http::Response::from_string("not found").with_status_code(404)
        } else {
            let p = shared.lock().unwrap_or_else(|e| e.into_inner());
            if p.at.elapsed() > stale {
                tiny_http::Response::from_string("the loop is stuck").with_status_code(503)
            } else {
                let body = serde_json::to_string(&p.status).unwrap_or_default();
                tiny_http::Response::from_string(body).with_header(tiny_http::Header {
                    field: "Content-Type".parse().expect("valid header name"),
                    value: "application/json".parse().expect("valid header value"),
                })
            }
        };
        let _ = request.respond(response);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Kind;

    struct Fake(HashMap<String, State>, Vec<String>);

    impl Runners for Fake {
        fn state(&mut self, r: &str) -> State {
            self.0.get(r).copied().unwrap_or(State::Down)
        }
        fn start(&mut self, r: &str) -> Result<(), String> {
            self.1.push(format!("start {r}"));
            Ok(())
        }
        fn stop(&mut self, r: &str) -> Result<(), String> {
            self.1.push(format!("stop {r}"));
            Ok(())
        }
    }

    fn fake(list: &[(&str, State)]) -> Fake {
        Fake(
            list.iter().map(|(r, s)| (r.to_string(), *s)).collect(),
            vec![],
        )
    }

    fn host(name: &str, runners: u32) -> Host {
        Host {
            name: name.into(),
            endpoint: "http://127.0.0.1:1".into(),
            runners,
            listen: None,
            kind: Kind::Docker,
            root: None,
        }
    }

    fn published(at: Instant) -> Shared {
        Arc::new(Mutex::new(Published {
            status: Status {
                host: "B".into(),
                runners: BTreeMap::from([("runner-B-1".into(), State::Busy)]),
            },
            at,
        }))
    }

    #[test]
    fn spills_over_to_our_second_runner_only_when_the_first_is_busy() {
        let hosts = [host("A", 2), host("B", 2)];
        let order = stages::order(&hosts);
        let mut f = fake(&[("runner-B-1", State::Stopped), ("runner-B-2", State::Idle)]);
        // A-1 busy: B-1 starts. It is idle now, so B-2 is not wanted and stops.
        let mut states = HashMap::from([("runner-A-1".to_string(), State::Busy)]);
        let ours = tick(
            &hosts[1],
            &order,
            &mut states,
            &mut f,
            &mut Memory::default(),
            Instant::now(),
        );
        assert_eq!(f.1, ["start runner-B-1", "stop runner-B-2"]);
        assert_eq!(ours["runner-B-1"], State::Idle);
        assert_eq!(ours["runner-B-2"], State::Stopped);
    }

    #[test]
    fn a_runner_that_fails_to_start_counts_as_down() {
        struct Broken;
        impl Runners for Broken {
            fn state(&mut self, _: &str) -> State {
                State::Stopped
            }
            fn start(&mut self, _: &str) -> Result<(), String> {
                Err("no such container".into())
            }
            fn stop(&mut self, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let hosts = [host("B", 1)];
        let mut states = HashMap::new();
        let ours = tick(
            &hosts[0],
            &stages::order(&hosts),
            &mut states,
            &mut Broken,
            &mut Memory::default(),
            Instant::now(),
        );
        assert_eq!(ours["runner-B-1"], State::Down);
    }

    #[test]
    fn a_runner_that_exits_right_after_start_is_held_down_so_the_next_one_runs() {
        let hosts = [host("A", 2)];
        let order = stages::order(&hosts);
        let mut f = fake(&[
            ("runner-A-1", State::Stopped),
            ("runner-A-2", State::Stopped),
        ]);
        let mut memory = Memory::default();
        let t0 = Instant::now();
        let ours = tick(
            &hosts[0],
            &order,
            &mut HashMap::new(),
            &mut f,
            &mut memory,
            t0,
        );
        assert_eq!(ours["runner-A-1"], State::Idle);
        assert_eq!(f.1, ["start runner-A-1"]);
        // Ten seconds later it has exited again.
        let t1 = t0 + Duration::from_secs(10);
        let ours = tick(
            &hosts[0],
            &order,
            &mut HashMap::new(),
            &mut f,
            &mut memory,
            t1,
        );
        assert_eq!(ours["runner-A-1"], State::Down);
        assert_eq!(f.1, ["start runner-A-1", "start runner-A-2"]);
        // After the hold it is tried again.
        let t2 = t1 + HOLD + Duration::from_secs(1);
        f.1.clear();
        f.0.insert("runner-A-2".into(), State::Idle);
        tick(
            &hosts[0],
            &order,
            &mut HashMap::new(),
            &mut f,
            &mut memory,
            t2,
        );
        assert_eq!(f.1, ["start runner-A-1", "stop runner-A-2"]);
    }

    #[test]
    fn does_not_stop_a_runner_that_took_a_job_since_the_first_look() {
        struct Racy(u32, Vec<String>);
        impl Runners for Racy {
            fn state(&mut self, _: &str) -> State {
                self.0 += 1;
                if self.0 == 1 {
                    State::Idle
                } else {
                    State::Busy
                }
            }
            fn start(&mut self, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn stop(&mut self, r: &str) -> Result<(), String> {
                self.1.push(r.into());
                Ok(())
            }
        }
        let hosts = [host("A", 1), host("B", 1)];
        let mut states = HashMap::from([("runner-A-1".to_string(), State::Idle)]);
        let mut racy = Racy(0, vec![]);
        let ours = tick(
            &hosts[1],
            &stages::order(&hosts),
            &mut states,
            &mut racy,
            &mut Memory::default(),
            Instant::now(),
        );
        assert!(racy.1.is_empty());
        assert_eq!(ours["runner-B-1"], State::Idle);
    }

    #[test]
    fn a_peer_speaks_only_for_its_own_runners() {
        let status = Status {
            host: "A".into(),
            runners: BTreeMap::from([
                ("runner-A-1".into(), State::Busy),
                ("runner-A-9".into(), State::Busy),
                ("runner-B-1".into(), State::Busy),
            ]),
        };
        let got: Vec<_> = own_runners(&host("A", 2), status).collect();
        assert_eq!(got, [("runner-A-1".to_string(), State::Busy)]);
    }

    fn served(shared: Shared, stale: Duration) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        std::thread::spawn(move || serve(server, shared, stale));
        format!("http://{addr}")
    }

    #[test]
    fn serves_its_status_and_reads_another() {
        let url = served(published(Instant::now()), Duration::from_secs(60));
        let got = fetch(&url).unwrap();
        assert_eq!(got.runners["runner-B-1"], State::Busy);
        assert!(fetch("http://127.0.0.1:1").is_err());
    }

    #[test]
    fn a_stuck_loop_answers_503() {
        let old = Instant::now() - Duration::from_secs(120);
        let url = served(published(old), Duration::from_secs(60));
        assert!(fetch(&url).unwrap_err().contains("503"));
    }
}
