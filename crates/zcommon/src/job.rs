use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

type Wake = Arc<dyn Fn() + Send + Sync>;
type OnPanic<M> = Arc<dyn Fn(String) -> M + Send + Sync>;

struct Run<M> {
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    rx: Receiver<M>,
}

pub struct Job<M> {
    wake: Wake,
    on_panic: Option<OnPanic<M>>,
    run: Option<Run<M>>,
}

pub struct JobCtx<M> {
    cancel: Arc<AtomicBool>,
    tx: Sender<M>,
    wake: Wake,
}

struct FinishGuard {
    running: Arc<AtomicBool>,
    wake: Wake,
}

impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        (self.wake)();
    }
}

impl<M: Send + 'static> Job<M> {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            wake: Arc::new(wake),
            on_panic: None,
            run: None,
        }
    }

    pub fn on_panic(mut self, message: impl Fn(String) -> M + Send + Sync + 'static) -> Self {
        self.on_panic = Some(Arc::new(message));
        self
    }

    pub fn start(&mut self, work: impl FnOnce(&JobCtx<M>) + Send + 'static) {
        self.cancel();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(true));
        let ctx = JobCtx {
            cancel: cancel.clone(),
            tx,
            wake: self.wake.clone(),
        };
        let guard = FinishGuard {
            running: running.clone(),
            wake: self.wake.clone(),
        };
        self.run = Some(Run {
            cancel,
            running,
            rx,
        });
        let on_panic = self.on_panic.clone();
        std::thread::spawn(move || {
            let _guard = guard;
            let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| work(&ctx)));
            if let (Err(payload), Some(on_panic)) = (panicked, on_panic) {
                ctx.send(on_panic(panic_message(payload.as_ref())));
            }
        });
    }

    pub fn cancel(&mut self) {
        if let Some(run) = self.run.take() {
            run.cancel.store(true, Ordering::SeqCst);
            run.running.store(false, Ordering::SeqCst);
        }
    }

    pub fn poll(&mut self) -> Vec<M> {
        match &self.run {
            Some(run) => run.rx.try_iter().collect(),
            None => Vec::new(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.running.load(Ordering::SeqCst))
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "worker panicked".to_string()
    }
}

impl<M> std::fmt::Debug for Job<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("started", &self.run.is_some())
            .finish()
    }
}

impl<M> Drop for Job<M> {
    fn drop(&mut self) {
        if let Some(run) = self.run.take() {
            run.cancel.store(true, Ordering::SeqCst);
        }
    }
}

impl<M> JobCtx<M> {
    pub fn send(&self, msg: M) -> bool {
        if self.is_cancelled() || self.tx.send(msg).is_err() {
            return false;
        }
        (self.wake)();
        true
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn cancel_flag(&self) -> &Arc<AtomicBool> {
        &self.cancel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out: {what}");
            std::thread::yield_now();
        }
    }

    fn collect(job: &mut Job<u32>, n: usize) -> Vec<u32> {
        let mut out = Vec::new();
        wait_until("messages", || {
            out.extend(job.poll());
            out.len() >= n
        });
        out
    }

    #[test]
    fn start_then_poll_returns_messages_in_order() {
        let mut job = Job::new(|| {});
        job.start(|ctx| {
            for i in 0..5 {
                ctx.send(i);
            }
        });
        assert_eq!(collect(&mut job, 5), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn second_start_drops_later_messages_of_first() {
        let mut job = Job::new(|| {});
        let (go_tx, go_rx) = channel::<()>();
        let (done_tx, done_rx) = channel::<bool>();
        job.start(move |ctx| {
            go_rx.recv().ok();
            done_tx.send(ctx.send(1)).ok();
        });
        job.start(|ctx| {
            ctx.send(2);
        });
        go_tx.send(()).unwrap();
        assert!(!done_rx.recv_timeout(Duration::from_secs(10)).unwrap());
        assert_eq!(collect(&mut job, 1), vec![2]);
        wait_until("finish", || !job.is_running());
        assert!(job.poll().is_empty());
    }

    #[test]
    fn cancel_makes_send_false_and_not_running() {
        let mut job: Job<u32> = Job::new(|| {});
        let (go_tx, go_rx) = channel::<()>();
        let (res_tx, res_rx) = channel::<bool>();
        job.start(move |ctx| {
            go_rx.recv().ok();
            res_tx.send(ctx.send(1)).ok();
        });
        assert!(job.is_running());
        job.cancel();
        assert!(!job.is_running());
        go_tx.send(()).unwrap();
        assert!(!res_rx.recv_timeout(Duration::from_secs(10)).unwrap());
        assert!(job.poll().is_empty());
    }

    #[test]
    fn is_running_false_when_worker_returns_without_sending() {
        let mut job: Job<u32> = Job::new(|| {});
        job.start(|_| {});
        wait_until("not running", || !job.is_running());
    }

    #[test]
    fn wake_fires_on_send_and_on_finish() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let mut job = Job::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        });
        job.start(|ctx| {
            ctx.send(1u32);
            ctx.send(2u32);
        });
        wait_until("wakes", || count.load(Ordering::SeqCst) >= 3);
        wait_until("not running", || !job.is_running());
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn dropping_job_cancels_worker() {
        let (go_tx, go_rx) = channel::<()>();
        let (res_tx, res_rx) = channel::<(bool, bool)>();
        let mut job: Job<u32> = Job::new(|| {});
        job.start(move |ctx| {
            go_rx.recv().ok();
            res_tx.send((ctx.is_cancelled(), ctx.send(1))).ok();
        });
        drop(job);
        go_tx.send(()).unwrap();
        assert_eq!(
            res_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            (true, false)
        );
    }

    #[test]
    fn cancel_flag_is_shared() {
        let (go_tx, go_rx) = channel::<()>();
        let (res_tx, res_rx) = channel::<bool>();
        let mut job: Job<u32> = Job::new(|| {});
        job.start(move |ctx| {
            let flag = ctx.cancel_flag().clone();
            go_rx.recv().ok();
            res_tx.send(flag.load(Ordering::SeqCst)).ok();
        });
        job.cancel();
        go_tx.send(()).unwrap();
        assert!(res_rx.recv_timeout(Duration::from_secs(10)).unwrap());
    }

    #[test]
    fn panicking_worker_ends_not_running() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let mut job: Job<u32> = Job::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        });
        job.start(|_| panic!("worker panic (expected by test)"));
        wait_until("wake after panic", || count.load(Ordering::SeqCst) >= 1);
        assert!(!job.is_running());
    }

    #[test]
    fn a_panicking_worker_sends_the_on_panic_message() {
        let mut job: Job<Result<u32, String>> = Job::new(|| {}).on_panic(Err);
        job.start(|_| panic!("worker panic (expected by test)"));
        wait_until("not running", || !job.is_running());
        assert_eq!(
            job.poll(),
            vec![Err("worker panic (expected by test)".to_string())]
        );
    }
}
