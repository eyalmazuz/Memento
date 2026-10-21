//! Real mpv subtitle notifications with furigana left enabled from startup.
use memento_app::{
    apply_subtitle_furigana, controller::subtitles::subtitle_furigana_parts,
    publish_active_subtitle, MementoApp,
};
use memento_player::mpv::PlaybackState;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

pub struct StartupSubtitles {
    requests: mpsc::Sender<(u64, String)>,
    results: mpsc::Receiver<(u64, String, Vec<(String, String)>)>,
    epoch: Arc<AtomicU64>,
    previous: (String, f64, f64),
    last_poll: Instant,
    pub publications: usize,
    pub ruby_updates: usize,
}

impl StartupSubtitles {
    pub fn new() -> Self {
        let (requests, rx) = mpsc::channel::<(u64, String)>();
        let (tx, results) = mpsc::channel();
        let epoch = Arc::new(AtomicU64::new(0));
        let worker_epoch = epoch.clone();
        std::thread::spawn(move || {
            while let Ok(mut request) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    request = newer;
                }
                if request.0 != worker_epoch.load(Ordering::Acquire) {
                    continue;
                }
                let parts = subtitle_furigana_parts(&request.1).expect("MeCab subtitle reading");
                if tx.send((request.0, request.1, parts)).is_err() {
                    break;
                }
            }
        });
        Self {
            requests,
            results,
            epoch,
            previous: (String::new(), 0., 0.),
            last_poll: Instant::now() - Duration::from_secs(1),
            publications: 0,
            ruby_updates: 0,
        }
    }

    pub fn tick(&mut self, app: &MementoApp, state: &PlaybackState) -> Vec<String> {
        let mut updates = Vec::new();
        // Use the same cadence and notification condition as the desktop timer.
        if self.last_poll.elapsed() >= Duration::from_millis(100) {
            self.last_poll = Instant::now();
            let current = (state.sub_text.clone(), state.sub_start, state.sub_end);
            if current != self.previous {
                publish_active_subtitle(app, &current.0, &self.requests, &self.epoch);
                self.publications += 1;
                updates.push(format!(
                    "mpv subtitle: {} chars, {:.2}..{:.2}",
                    current.0.chars().count(),
                    current.1,
                    current.2
                ));
                self.previous = current;
            }
        }
        while let Ok((epoch, text, parts)) = self.results.try_recv() {
            if epoch != self.epoch.load(Ordering::Acquire)
                || app.get_active_sub_text().as_str() != text
            {
                continue;
            }
            apply_subtitle_furigana(app, &text, parts);
            self.ruby_updates += 1;
            updates.push("startup furigana ready".into());
        }
        updates
    }
}
