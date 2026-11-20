//! Background ASR worker loop, incremental cue emission, idle model unloading,
//! and abortable retry/wait coordination.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use memento_asr::asrcontroller::{adjust_segment, is_cue_valid};

use crate::controller::media::{extract_audio_samples, MediaContext as AsrMedia};

use super::cue_book::{CueBook, Window, WindowPlan};
use super::engines::{
    load_runtime_model, transcribe_window, ResolvedModel, RuntimeModel, Transcription,
};
use super::{AsrCue, AsrEvent, AsrSettings};

const MAX_TRANSCRIPTION_FAILURES: u32 = 3;
pub(super) const DEFAULT_IDLE_UNLOAD_TIMEOUT: Duration = Duration::from_secs(45);
const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
pub(super) struct WorkerTiming {
    pub(super) idle_unload_timeout: Duration,
    pub(super) retry_backoff: Duration,
}

impl Default for WorkerTiming {
    fn default() -> Self {
        Self {
            idle_unload_timeout: DEFAULT_IDLE_UNLOAD_TIMEOUT,
            retry_backoff: DEFAULT_RETRY_BACKOFF,
        }
    }
}

#[derive(Default)]
pub(super) struct Control {
    pub(super) stop: bool,
    pub(super) active: bool,
    pub(super) suspended: bool,
    pub(super) parked_no_audio: bool,
    pub(super) paused_on_errors: bool,
    pub(super) done: bool,
    pub(super) completed_duration: f64,
    pub(super) reset_cues: bool,
    pub(super) emit_snapshot: bool,
    pub(super) media_path: String,
    pub(super) audio_id: i64,
    pub(super) generation: u64,
    pub(super) min_cue_generation: u64,
    pub(super) duration: f64,
    pub(super) position: f64,
    pub(super) last_position: Option<f64>,
    pub(super) last_position_at: Option<Instant>,
    pub(super) last_paused: bool,
    pub(super) suspended_at_position: Option<f64>,
    pub(super) current_window: Option<(f64, f64)>,
    pub(super) restart_position: Option<f64>,
    pub(super) current_abort: Option<Arc<AtomicBool>>,
}

pub(super) struct Shared {
    pub(super) control: Mutex<Control>,
    pub(super) latest_cues: Mutex<(u64, Vec<AsrCue>)>,
    pub(super) wake: Condvar,
    pub(super) running: AtomicBool,
}

pub(super) struct JoinOnDrop(pub(super) Option<JoinHandle<()>>);

impl JoinOnDrop {
    pub(super) fn join_now(&mut self) {
        if let Some(handle) = self.0.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for JoinOnDrop {
    fn drop(&mut self) {
        self.join_now();
    }
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) struct SegmentContext<'a> {
    pub(super) generation: u64,
    window_start: f64,
    window_end: f64,
    abort: &'a AtomicBool,
    cues: &'a mut CueBook,
    shared: &'a Shared,
    sender: &'a mpsc::Sender<AsrEvent>,
}

impl SegmentContext<'_> {
    pub(super) fn emit_segment(&mut self, text: &str, seg_start: f64, seg_end: f64) -> bool {
        if self.abort.load(Ordering::Acquire) {
            return false;
        }
        {
            let control = lock(&self.shared.control);
            if control.stop || !control.active || self.generation < control.min_cue_generation {
                return false;
            }
        }
        let text = text.trim().to_owned();
        let adjusted = adjust_segment(seg_start, seg_end, self.window_start, self.window_end);
        if is_cue_valid(!text.is_empty(), adjusted.start, adjusted.end) {
            let delta = self.cues.insert(text, adjusted.start, adjusted.end);
            {
                let mut guard = lock(&self.shared.latest_cues);
                if self.generation < guard.0 {
                    return false;
                }
                guard.0 = self.generation;
                if !delta.removed.is_empty() {
                    guard
                        .1
                        .retain(|existing| !delta.removed.contains(&existing.id));
                }
                let insertion = guard
                    .1
                    .partition_point(|existing| existing.start < delta.added.start);
                guard.1.insert(insertion, delta.added.clone());
            }
            for cue_id in delta.removed {
                let _ = self.sender.send(AsrEvent::CueRemoved {
                    generation: self.generation,
                    cue_id,
                });
            }
            let _ = self.sender.send(AsrEvent::CueAdded {
                generation: self.generation,
                cue: delta.added,
            });
        }
        true
    }
}

pub(super) enum WindowError {
    /// Audio extraction failed for the current window; ignored if superseded.
    Extract(String),
    /// Lazy model reload failed; fatal unless the session has been stopped.
    ModelLoad(String),
}

pub(super) trait WindowProcessor {
    fn is_model_loaded(&self) -> bool;
    fn unload_model(&mut self);
    fn reset_media_state(&mut self);
    fn process_window(
        &mut self,
        media: &AsrMedia,
        window: &Window,
        abort: &Arc<AtomicBool>,
        seg_ctx: &mut SegmentContext<'_>,
    ) -> Result<Transcription, WindowError>;
}

struct RuntimeProcessor<'a> {
    shared: &'a Shared,
    settings: &'a AsrSettings,
    resolved: &'a ResolvedModel,
    model: Option<RuntimeModel>,
}

impl WindowProcessor for RuntimeProcessor<'_> {
    fn is_model_loaded(&self) -> bool {
        self.model.is_some()
    }

    fn unload_model(&mut self) {
        if let Some(model) = self.model.take() {
            drop(model);
            crate::app::trim_os_heap();
        }
    }

    fn reset_media_state(&mut self) {
        if let Some(model) = self.model.as_mut() {
            model.reset_vad_state();
        }
    }

    fn process_window(
        &mut self,
        media: &AsrMedia,
        window: &Window,
        abort: &Arc<AtomicBool>,
        seg_ctx: &mut SegmentContext<'_>,
    ) -> Result<Transcription, WindowError> {
        let samples = extract_audio_samples(media, window.start, window.end, abort)
            .map_err(WindowError::Extract)?;
        if abort.load(Ordering::Acquire) {
            return Ok(Transcription::Canceled);
        }
        if samples.is_empty() {
            self.reset_media_state();
            return Ok(Transcription::Success);
        }
        if self.model.is_none() {
            if is_stale_or_stopped(self.shared, seg_ctx.generation) {
                return Ok(Transcription::Canceled);
            }
            let loaded = match load_runtime_model(self.settings, self.resolved) {
                Ok(model) => model,
                Err(message) => {
                    let control = lock(&self.shared.control);
                    if control.stop || !control.active {
                        return Ok(Transcription::Canceled);
                    }
                    return Err(WindowError::ModelLoad(message));
                }
            };
            self.model = Some(loaded);
            if abort.load(Ordering::Acquire) || is_stale_or_stopped(self.shared, seg_ctx.generation)
            {
                return Ok(Transcription::Canceled);
            }
        }
        let model = self.model.as_mut().expect("model loaded above");
        Ok(transcribe_window(model, &samples, abort, seg_ctx, window))
    }
}

pub(super) fn run_session(
    shared: Arc<Shared>,
    sender: mpsc::Sender<AsrEvent>,
    settings: AsrSettings,
    media: AsrMedia,
    resolved: ResolvedModel,
    initial_position: f64,
    mut prev_worker: JoinOnDrop,
) {
    // Wait for any stopping previous worker to release its native/CUDA model buffers
    // on this background thread before allocating the new model.
    prev_worker.join_now();

    let generation = {
        let control = lock(&shared.control);
        if control.stop || !control.active {
            shared.running.store(false, Ordering::Release);
            return;
        }
        control.generation
    };
    let _ = sender.send(AsrEvent::Started { generation });
    let model = match load_runtime_model(&settings, &resolved) {
        Ok(model) => model,
        Err(message) => {
            finish_with_error(&shared, &sender, message);
            return;
        }
    };
    let mut processor = RuntimeProcessor {
        shared: &shared,
        settings: &settings,
        resolved: &resolved,
        model: Some(model),
    };
    run_session_loop(
        shared.clone(),
        sender,
        media,
        initial_position,
        generation,
        WorkerTiming::default(),
        &mut processor,
    );
    processor.unload_model();
}

pub(super) fn run_session_loop<P: WindowProcessor>(
    shared: Arc<Shared>,
    sender: mpsc::Sender<AsrEvent>,
    mut media: AsrMedia,
    initial_position: f64,
    initial_generation: u64,
    timing: WorkerTiming,
    processor: &mut P,
) {
    let mut cues = CueBook::default();
    let mut seen_generation = initial_generation;
    let mut start = initial_position;
    let mut rewound = false;
    let mut failures: u32 = 0;

    loop {
        if !wait_for_selection(&shared, timing.idle_unload_timeout, processor) {
            break;
        }
        let (
            currently_parked,
            active_generation,
            duration,
            position,
            restart_position,
            reset_cues,
            new_media_path,
            new_audio_id,
            emit_snapshot,
        ) = {
            let mut control = lock(&shared.control);
            if control.stop || !control.active {
                break;
            }
            let parked = control.suspended || control.parked_no_audio;
            let generation = control.generation;
            let restart = if !parked && generation != seen_generation {
                control.restart_position.take()
            } else {
                None
            };
            let reset_cues = std::mem::replace(&mut control.reset_cues, false);
            let emit_snapshot = if !parked {
                std::mem::replace(&mut control.emit_snapshot, false)
            } else {
                false
            };
            (
                parked,
                generation,
                control.duration,
                control.position,
                restart,
                reset_cues,
                control.media_path.clone(),
                control.audio_id,
                emit_snapshot,
            )
        };
        if reset_cues {
            if !new_media_path.is_empty() {
                media.path = new_media_path;
            }
            media.audio_id = new_audio_id;
            processor.reset_media_state();
            cues.clear_all(active_generation, &sender);
            let mut guard = lock(&shared.latest_cues);
            if active_generation >= guard.0 {
                *guard = (active_generation, Vec::new());
            }
        } else if emit_snapshot {
            let _ = sender.send(AsrEvent::CuesSnapshot {
                generation: active_generation,
                cues: cues.cues.clone(),
            });
        }
        if currently_parked {
            shared.running.store(false, Ordering::Release);
            continue;
        }
        if active_generation != seen_generation {
            seen_generation = active_generation;
            start = restart_position.unwrap_or(position).clamp(0.0, duration);
            rewound = false;
            failures = 0;
            let _ = sender.send(AsrEvent::Started {
                generation: seen_generation,
            });
        }

        let plan = cues.plan_window(start, duration, rewound);
        match plan {
            WindowPlan::Done => {
                shared.running.store(false, Ordering::Release);
                let mut control = lock(&shared.control);
                if control.stop
                    || !control.active
                    || control.suspended
                    || control.parked_no_audio
                    || control.reset_cues
                    || control.emit_snapshot
                    || control.generation != seen_generation
                {
                    continue;
                }
                let _ = sender.send(AsrEvent::Completed {
                    generation: seen_generation,
                    cue_count: cues.cues.len(),
                });
                control.done = true;
                control.completed_duration = duration;
                let mut control = wait_while_idle(
                    &shared,
                    control,
                    timing.idle_unload_timeout,
                    |c| {
                        !c.stop
                            && c.active
                            && !c.reset_cues
                            && !c.emit_snapshot
                            && c.generation == seen_generation
                    },
                    || {},
                    processor,
                );
                control.done = false;
                control.completed_duration = 0.0;
                continue;
            }
            WindowPlan::Advance {
                start: next,
                rewound: was_rewound,
            } => {
                start = next;
                rewound = was_rewound;
                continue;
            }
            WindowPlan::Extract(window) => {
                start = window.start;
                rewound = window.rewound;
                shared.running.store(true, Ordering::Release);
                let _ = sender.send(AsrEvent::WindowStarted {
                    generation: seen_generation,
                    start: window.start,
                    end: window.end,
                });
                let abort = Arc::new(AtomicBool::new(false));
                {
                    let mut control = lock(&shared.control);
                    if control.stop
                        || control.suspended
                        || control.parked_no_audio
                        || control.generation != seen_generation
                    {
                        continue;
                    }
                    control.current_window = Some((window.start, window.end));
                    control.current_abort = Some(abort.clone());
                }
                let mut seg_ctx = SegmentContext {
                    generation: seen_generation,
                    window_start: window.start,
                    window_end: window.end,
                    abort: &abort,
                    cues: &mut cues,
                    shared: &shared,
                    sender: &sender,
                };
                let status = match processor.process_window(&media, &window, &abort, &mut seg_ctx) {
                    Ok(status) => status,
                    Err(WindowError::Extract(message)) => {
                        clear_attempt(&shared, &abort);
                        if is_stale_or_stopped(&shared, seen_generation) {
                            continue;
                        }
                        finish_with_error(&shared, &sender, message);
                        return;
                    }
                    Err(WindowError::ModelLoad(message)) => {
                        clear_attempt(&shared, &abort);
                        {
                            let control = lock(&shared.control);
                            if control.stop || !control.active {
                                continue;
                            }
                        }
                        finish_with_error(&shared, &sender, message);
                        return;
                    }
                };
                clear_attempt(&shared, &abort);
                match status {
                    Transcription::Success => {
                        failures = 0;
                        // Record silent windows too: they have no cues, but VAD
                        // must never run on them a second time this session.
                        cues.mark_processed(window.start, window.end);
                        if is_stale_or_stopped(&shared, seen_generation) {
                            continue;
                        }
                        start = window.end;
                    }
                    Transcription::Canceled => {
                        if is_stale_or_stopped(&shared, seen_generation) {
                            continue;
                        }
                        let control = lock(&shared.control);
                        if control.generation == seen_generation && !control.stop {
                            start = control.position.clamp(0.0, control.duration);
                        }
                    }
                    Transcription::Failed(message) => {
                        if is_stale_or_stopped(&shared, seen_generation) {
                            continue;
                        }
                        failures += 1;
                        if failures >= MAX_TRANSCRIPTION_FAILURES {
                            shared.running.store(false, Ordering::Release);
                            let mut control = lock(&shared.control);
                            if control.stop
                                || !control.active
                                || control.reset_cues
                                || control.generation != seen_generation
                            {
                                continue;
                            }
                            let _ = sender.send(AsrEvent::Paused {
                                generation: seen_generation,
                                message: format!(
                                    "ASR paused after repeated transcription failures: {message}. Seek or change ASR settings to retry."
                                ),
                            });
                            control.paused_on_errors = true;
                            let mut control = wait_while_idle(
                                &shared,
                                control,
                                timing.idle_unload_timeout,
                                |c| {
                                    !c.stop
                                        && c.active
                                        && !c.reset_cues
                                        && c.generation == seen_generation
                                },
                                || {},
                                processor,
                            );
                            control.paused_on_errors = false;
                        } else if let Some(next_start) = wait_retry_backoff(
                            &shared,
                            seen_generation,
                            timing.retry_backoff.saturating_mul(failures),
                        ) {
                            start = next_start;
                        }
                    }
                }
            }
        }
    }
    shared.running.store(false, Ordering::Release);
    let generation = lock(&shared.control).generation;
    let _ = sender.send(AsrEvent::Canceled { generation });
    let mut control = lock(&shared.control);
    control.active = false;
    control.current_abort = None;
    control.current_window = None;
}

/// Wait while idle (suspended, parked without audio, done, or paused on errors),
/// unloading the native model after `idle_timeout` without holding `shared.control`
/// so UI threads never stall on multi-GB model/CUDA destruction.
fn wait_while_idle<'a, F, W, P: WindowProcessor>(
    shared: &'a Shared,
    mut control: MutexGuard<'a, Control>,
    idle_timeout: Duration,
    mut should_wait: F,
    mut on_wait: W,
    processor: &mut P,
) -> MutexGuard<'a, Control>
where
    F: FnMut(&Control) -> bool,
    W: FnMut(),
{
    if !should_wait(&control) {
        return control;
    }
    let mut idle_deadline = processor
        .is_model_loaded()
        .then(|| Instant::now() + idle_timeout);
    while should_wait(&control) {
        on_wait();
        if let Some(deadline) = idle_deadline {
            let now = Instant::now();
            if now >= deadline {
                drop(control);
                processor.unload_model();
                idle_deadline = None;
                control = lock(&shared.control);
                continue;
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next, wait_result) = shared
                .wake
                .wait_timeout(control, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            control = next;
            if wait_result.timed_out() && should_wait(&control) {
                drop(control);
                processor.unload_model();
                idle_deadline = None;
                control = lock(&shared.control);
            }
        } else {
            control = shared
                .wake
                .wait(control)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
    control
}

/// Brief abortable backoff between transient transcription retries so a paused
/// player does not tight-spin at 0ms intervals while still waking immediately
/// on stop, seek, suspend, or track/media changes.
fn wait_retry_backoff(shared: &Shared, seen_generation: u64, backoff: Duration) -> Option<f64> {
    let mut control = lock(&shared.control);
    if !backoff.is_zero() {
        let deadline = Instant::now() + backoff;
        while !control.stop
            && control.active
            && !control.suspended
            && !control.parked_no_audio
            && !control.reset_cues
            && control.generation == seen_generation
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next, result) = shared
                .wake
                .wait_timeout(control, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            control = next;
            if result.timed_out() {
                break;
            }
        }
    }
    if control.stop
        || !control.active
        || control.suspended
        || control.parked_no_audio
        || control.reset_cues
        || control.generation != seen_generation
    {
        None
    } else {
        Some(control.position.clamp(0.0, control.duration))
    }
}

/// Wait between windows while suspended or parked without audio, retaining
/// `CueBook` and unloading the heavy native model after `idle_timeout`.
pub(super) fn wait_for_selection<P: WindowProcessor>(
    shared: &Shared,
    idle_timeout: Duration,
    processor: &mut P,
) -> bool {
    let control = lock(&shared.control);
    let control = wait_while_idle(
        shared,
        control,
        idle_timeout,
        |c| c.active && !c.stop && (c.suspended || c.parked_no_audio) && !c.reset_cues,
        || shared.running.store(false, Ordering::Release),
        processor,
    );
    control.active && !control.stop
}

pub(super) fn finish_with_error(shared: &Shared, sender: &mpsc::Sender<AsrEvent>, message: String) {
    shared.running.store(false, Ordering::Release);
    // Read the latest generation under lock so a model-load failure is never
    // silently dropped by `poll()` if a seek/file/track switch occurred mid-load.
    let (generation, stopped) = {
        let mut control = lock(&shared.control);
        let generation = control.generation;
        let stopped = control.stop;
        control.active = false;
        control.current_abort = None;
        control.current_window = None;
        (generation, stopped)
    };
    if !stopped {
        let _ = sender.send(AsrEvent::Failed {
            generation,
            message,
        });
    }
}

fn is_stale_or_stopped(shared: &Shared, generation: u64) -> bool {
    let control = lock(&shared.control);
    control.stop || !control.active || control.generation != generation
}

fn clear_attempt(shared: &Shared, abort: &Arc<AtomicBool>) {
    let mut control = lock(&shared.control);
    if control
        .current_abort
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, abort))
    {
        control.current_abort = None;
        control.current_window = None;
    }
}
