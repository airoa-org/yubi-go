use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use log::{info, warn};

use super::gc::{gc_tick, housekeeping};
use super::state::{self, State};

/// How long to wait after a failure, and the ceiling it doubles to. Held on
/// the queue rather than per take, because what fails is almost always the
/// network. Chosen by hand.
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// How often to look for work when there is none: one `read_dir` of the
/// takes directory, so it is cheap. Chosen by hand.
const IDLE_POLL: Duration = Duration::from_secs(15);

/// How often to look again while a take is recording. A take shares the
/// radio and the CPU with an upload, so nothing is sent until it closes.
const BUSY_POLL: Duration = Duration::from_secs(3);

/// `recording` must stay true until a stopped take has closed (`Hub::busy`).
/// It is asked again between files and parts, so an
/// upload under way stops when a take starts.
pub fn install(dir: String, recording: impl Fn() -> bool + Send + 'static) {
    std::thread::spawn(move || {
        let mut backoff = FIRST_BACKOFF;
        let mut said: Option<String> = None;
        let mut last_gc: Option<std::time::Instant> = None;
        let mut last_housekeeping: Option<std::time::Instant> = None;
        // Nameless until storage.yaml is readable, which needs airoa_core up.
        let mut upload_state = airoa_sdk::storage::UploadState::default();
        let mut failed: HashMap<PathBuf, Instant> = HashMap::new();
        loop {
            state::state_for(&dir, &mut upload_state);
            gc_tick(&dir, &mut last_gc, &recording, &upload_state);
            housekeeping(&dir, &mut last_housekeeping, &recording, &upload_state);
            let Some(target) = super::config::target(&dir, &mut said) else {
                state::DEPTH.store(0, std::sync::atomic::Ordering::Relaxed);
                std::thread::sleep(IDLE_POLL);
                continue;
            };
            let queue = state::pending(&dir, &upload_state);
            state::DEPTH.store(queue.len(), std::sync::atomic::Ordering::Relaxed);
            failed.retain(|take, _| queue.contains(take));
            let Some(episode) = next(&queue, &failed) else {
                if !recording() {
                    state::set(super::episode::probe(&target));
                }
                std::thread::sleep(IDLE_POLL);
                continue;
            };
            if recording() {
                std::thread::sleep(BUSY_POLL);
                continue;
            }
            let Some(_sending) = state::sending(episode) else { continue };
            let sent = super::episode::send(&target, episode, &recording);
            match sent {
                Ok((prefix, etags)) => {
                    backoff = FIRST_BACKOFF;
                    failed.remove(episode);
                    super::episode::settle(&target, episode, &prefix, &etags, &upload_state);
                }
                Err(e) if e == super::s3::PAUSED => {
                    info!("upload: {} paused for a take — sent again after it", episode.display());
                }
                Err(e) => {
                    warn!("upload: {} not sent: {e}", episode.display());
                    state::set(State::Retrying(e));
                    failed.insert(episode.clone(), Instant::now());
                    if wait_unless_storage_changes(&dir, backoff) {
                        backoff = FIRST_BACKOFF;
                        failed.clear();
                    } else {
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                }
            }
        }
    });
}

/// Sleeps out a backoff, but answers true as soon as `storage.yaml` is edited
/// or removed, so turning uploads off or fixing the endpoint takes effect now
/// rather than after a wait of up to `MAX_BACKOFF`.
fn wait_unless_storage_changes(dir: &str, backoff: Duration) -> bool {
    let before = super::config::storage_modified(dir);
    let until = Instant::now() + backoff;
    while Instant::now() < until {
        std::thread::sleep(Duration::from_secs(1).min(until - Instant::now()));
        if super::config::storage_modified(dir) != before {
            return true;
        }
    }
    false
}

/// The take to send next: the queue's order, except that a take that failed
/// waits behind every take that has not, the oldest failure first. So a take
/// the bucket always refuses holds up no other one, and is still retried.
fn next<'a>(queue: &'a [PathBuf], failed: &HashMap<PathBuf, Instant>) -> Option<&'a PathBuf> {
    queue.iter().min_by_key(|take| failed.get(*take).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_take_goes_behind_the_others_and_is_never_dropped() {
        let queue: Vec<PathBuf> = ["episode_1", "episode_2", "episode_3"].map(PathBuf::from).into();
        let mut failed = HashMap::new();
        assert_eq!(next(&queue, &failed), Some(&queue[0]), "the backlog first");

        let start = Instant::now();
        failed.insert(queue[0].clone(), start);
        assert_eq!(next(&queue, &failed), Some(&queue[1]), "the order is otherwise kept");

        failed.insert(queue[1].clone(), start + Duration::from_secs(1));
        failed.insert(queue[2].clone(), start + Duration::from_secs(2));
        assert_eq!(next(&queue, &failed), Some(&queue[0]), "all failed: the oldest failure");
    }
}
