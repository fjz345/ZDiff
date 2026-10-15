use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

pub fn hash_contents(contents: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update_rayon(contents);
    hasher.finalize().to_hex().to_string()
}

/// blake3 measures rayon helping from about 128 KiB.
const RAYON_MIN_FILE_SIZE: u64 = 128 * 1024;

pub fn hash_file_mmap(path: impl AsRef<Path>) -> io::Result<String> {
    let path = path.as_ref();
    let mut hasher = blake3::Hasher::new();
    if std::fs::metadata(path)?.len() >= RAYON_MIN_FILE_SIZE {
        hasher.update_mmap_rayon(path)?;
    } else {
        hasher.update_mmap(path)?;
    }
    Ok(hasher.finalize().to_hex().to_string())
}

const WORKER_WAIT: Duration = Duration::from_millis(50);

pub type HashRepresentation = String;

#[derive(Debug, Clone)]
pub struct HashServiceSnapshot {
    pub hashes: HashMap<PathBuf, Option<HashRepresentation>>,
    pub active_count: usize,
    pub queue_count: usize,
    pub num_workers: usize,
}

#[derive(Debug)]
pub struct HashService {
    hashes: Arc<RwLock<HashMap<PathBuf, Option<HashRepresentation>>>>,
    tx: Sender<PathBuf>,
    rx: Arc<Mutex<Receiver<PathBuf>>>,
    queued: Arc<AtomicUsize>,
    in_progress: Arc<AtomicUsize>,
    draining: Arc<AtomicUsize>,
    workers: Vec<WorkerHandle>,
}

#[derive(Debug)]
struct WorkerHandle {
    handle: thread::JoinHandle<()>,
    stop_flag: Arc<AtomicBool>,
}

impl Default for HashService {
    fn default() -> Self {
        HashService::new(4)
    }
}

impl HashService {
    pub fn new(worker_count: usize) -> Self {
        let (tx, rx) = mpsc::channel::<PathBuf>();
        let rx = Arc::new(Mutex::new(rx));

        let hashes = Arc::new(RwLock::new(HashMap::new()));
        let in_progress = Arc::new(AtomicUsize::new(0));

        let mut service = Self {
            hashes,
            tx,
            rx,
            queued: Arc::new(AtomicUsize::new(0)),
            in_progress,
            draining: Arc::new(AtomicUsize::new(0)),
            workers: Vec::new(),
        };

        service.resize_workers(worker_count);
        service
    }

    fn spawn_worker(&mut self, id: usize) {
        let rx = self.rx.clone();
        let hashes = self.hashes.clone();
        let queued = self.queued.clone();
        let in_progress = self.in_progress.clone();
        let draining = self.draining.clone();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let thread_stop_flag = stop_flag.clone();

        let handle = thread::spawn(move || {
            log::info!("Worker {id} started");

            loop {
                if thread_stop_flag.load(Ordering::SeqCst) {
                    log::info!("Worker {id} stop signal");
                    break;
                }

                while draining.load(Ordering::SeqCst) > 0 {
                    thread::sleep(Duration::from_millis(1));
                }

                let msg = {
                    let rx_guard = rx.lock().unwrap();
                    if thread_stop_flag.load(Ordering::SeqCst) {
                        continue;
                    }
                    rx_guard.recv_timeout(WORKER_WAIT)
                };

                match msg {
                    Ok(path) => {
                        in_progress.fetch_add(1, Ordering::SeqCst);
                        queued.fetch_sub(1, Ordering::SeqCst);
                        let hash = hash_file_mmap(&path).ok();
                        hashes.write().unwrap().insert(path, hash);
                        in_progress.fetch_sub(1, Ordering::SeqCst);
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => {
                        log::info!("Worker {id} channel disconnected");
                        break;
                    }
                }
            }

            log::info!("Worker {id} exited");
        });

        self.workers.push(WorkerHandle { handle, stop_flag });
    }

    pub fn resize_workers(&mut self, new_count: usize) {
        let current = self.workers.len();

        log::info!("Resizing workers from {current} -> {new_count}");
        if new_count > current {
            for id in current..new_count {
                self.spawn_worker(id);
            }
        } else if new_count < current {
            let remove_count = current - new_count;

            for worker in self.workers.iter().rev().take(remove_count) {
                worker.stop_flag.store(true, Ordering::SeqCst);
            }

            for _ in 0..remove_count {
                if let Some(worker) = self.workers.pop() {
                    let _ = worker.handle.join();
                }
            }
        }
    }

    /// Returns whether `path` was queued. A known path (hashed, queued or failed) is not
    /// queued again until it is removed, cancelled or cleared.
    pub fn request(&self, path: impl AsRef<Path>) -> bool {
        let mut hashes = self.hashes.write().unwrap();
        if hashes.contains_key(path.as_ref()) {
            return false;
        }
        hashes.insert(path.as_ref().into(), None);
        self.queued.fetch_add(1, Ordering::SeqCst);
        self.tx
            .send(path.as_ref().into())
            .expect("the service holds a receiver");
        true
    }

    /// Drops the queued paths; hashes in progress still finish.
    pub fn cancel(&self) {
        // hashes before rx: request() holds hashes while sending, workers never hold rx
        // while waiting for hashes.
        let mut hashes = self.hashes.write().unwrap();
        // Workers wait on rx and std mutexes are unfair: make them back off so this lock is won.
        self.draining.fetch_add(1, Ordering::SeqCst);
        let rx = self.rx.lock().unwrap();
        while let Ok(path) = rx.try_recv() {
            self.queued.fetch_sub(1, Ordering::SeqCst);
            hashes.remove(&path);
        }
        self.draining.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn remove(&self, path: impl AsRef<Path>) {
        if let Ok(mut hashes) = self.hashes.write() {
            hashes.remove(path.as_ref());
        }
    }

    // TODO: fix to get reference instead of clone
    pub fn get_hash(&self, path: impl AsRef<Path>) -> Option<HashRepresentation> {
        let binding = self.hashes.read().unwrap();
        let hash = binding.get(path.as_ref().into()).and_then(|f| f.clone());
        hash
    }

    pub fn clear(&self) {
        self.cancel();
        self.hashes.write().unwrap().clear();
    }

    pub fn count_threads(&self) -> usize {
        self.workers.len()
    }

    pub fn count_active_hashes(&self) -> usize {
        self.in_progress.load(Ordering::SeqCst)
    }

    pub fn count_hash_queue(&self) -> usize {
        self.queued.load(Ordering::SeqCst)
    }

    pub fn snapshot(&self) -> HashServiceSnapshot {
        HashServiceSnapshot {
            hashes: self.hashes.read().unwrap().clone(),
            active_count: self.count_active_hashes(),
            queue_count: self.count_hash_queue(),
            num_workers: self.count_threads(),
        }
    }
}

impl Drop for HashService {
    fn drop(&mut self) {
        self.resize_workers(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::Instant;
    use tempfile::tempdir;

    #[test]
    fn test_service_basic_hashing() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "hello world").unwrap();

        let service = HashService::new(1);
        service.request(&file_path);

        let mut hash = None;
        for _ in 0..20 {
            hash = service.get_hash(&file_path);
            if hash.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        assert!(hash.is_some());
        assert_eq!(hash.unwrap(), hash_file_mmap(&file_path).unwrap());
    }

    #[test]
    fn file_hash_matches_the_content_hash_below_and_above_the_rayon_cutoff() {
        let dir = tempdir().unwrap();
        for size in [0, 10, RAYON_MIN_FILE_SIZE as usize + 1] {
            let contents: Vec<u8> = (0..size).map(|i| i as u8).collect();
            let path = dir.path().join(format!("{size}.bin"));
            fs::write(&path, &contents).unwrap();
            assert_eq!(hash_file_mmap(&path).unwrap(), hash_contents(&contents));
        }
    }

    #[test]
    fn test_worker_resizing() {
        let mut service = HashService::new(2);
        assert_eq!(service.count_threads(), 2);

        service.resize_workers(4);
        assert_eq!(service.count_threads(), 4);

        service.resize_workers(1);
        assert_eq!(service.count_threads(), 1);
    }

    #[test]
    fn test_queue_and_snapshot_counts() {
        let dir = tempdir().unwrap();
        let service = HashService::new(0); // 0 workers to freeze the queue

        let paths: Vec<PathBuf> = (0..5)
            .map(|i| {
                let p = dir.path().join(format!("{}.txt", i));
                File::create(&p).unwrap();
                p
            })
            .collect();

        for p in &paths {
            service.request(p);
        }

        let snap = service.snapshot();
        assert_eq!(snap.queue_count, 5);
        assert_eq!(snap.active_count, 0);
        assert_eq!(snap.hashes.len(), 5);

        // Ensure they are all pending (None)
        assert!(snap.hashes.values().all(|v| v.is_none()));
    }

    #[test]
    fn test_clear_and_remove() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("remove_me.txt");
        File::create(&file_path).unwrap();

        let service = HashService::new(1);
        service.request(&file_path);

        service.remove(&file_path);
        assert!(service.get_hash(&file_path).is_none());

        service.request(&file_path);
        service.clear();
        assert_eq!(service.snapshot().hashes.len(), 0);
    }

    fn files(dir: &Path, count: usize) -> Vec<PathBuf> {
        (0..count)
            .map(|i| {
                let p = dir.join(format!("{i}.txt"));
                fs::write(&p, format!("{i}")).unwrap();
                p
            })
            .collect()
    }

    fn wait_for_hash(service: &HashService, path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while service.get_hash(path).is_none() {
            assert!(Instant::now() < deadline, "{} not hashed", path.display());
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn cancel_drops_the_queued_hashes() {
        let dir = tempdir().unwrap();
        let paths = files(dir.path(), 3);
        let mut service = HashService::new(0);
        for p in &paths {
            assert!(service.request(p));
        }

        service.cancel();

        assert_eq!(service.count_hash_queue(), 0);
        assert!(service.snapshot().hashes.is_empty());

        service.resize_workers(1);
        let late = dir.path().join("late.txt");
        fs::write(&late, "late").unwrap();
        service.request(&late);
        wait_for_hash(&service, &late);
        let hashes = service.snapshot().hashes;
        assert_eq!(hashes.keys().collect::<Vec<_>>(), [&late]);
    }

    #[test]
    fn a_cancelled_file_can_be_requested_again() {
        let dir = tempdir().unwrap();
        let paths = files(dir.path(), 1);
        let mut service = HashService::new(0);
        service.request(&paths[0]);
        service.cancel();

        assert!(service.request(&paths[0]));
        service.resize_workers(1);
        wait_for_hash(&service, &paths[0]);
    }

    #[test]
    fn clear_drops_the_queued_hashes_too() {
        let dir = tempdir().unwrap();
        let paths = files(dir.path(), 2);
        let mut service = HashService::new(0);
        for p in &paths {
            service.request(p);
        }

        service.clear();
        for p in &paths {
            service.request(p);
        }

        assert_eq!(service.count_hash_queue(), 2);
        service.resize_workers(1);
        for p in &paths {
            wait_for_hash(&service, p);
        }
        assert_eq!(service.count_hash_queue(), 0);
        assert_eq!(service.count_active_hashes(), 0);
    }

    #[test]
    fn a_file_that_fails_to_hash_leaves_the_queue() {
        let dir = tempdir().unwrap();
        let mut service = HashService::new(0);
        service.request(dir.path().join("missing.txt"));
        let paths = files(dir.path(), 1);
        service.request(&paths[0]);

        service.resize_workers(1);
        wait_for_hash(&service, &paths[0]);

        assert_eq!(service.count_hash_queue(), 0);
    }

    #[test]
    fn test_duplicate_requests_ignored() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("dup.txt");
        File::create(&file_path).unwrap();

        let service = HashService::new(1);
        service.request(&file_path);
        service.request(&file_path);

        assert_eq!(service.snapshot().hashes.len(), 1);
    }

    #[test]
    fn idle_workers_stop_promptly_on_resize_to_zero() {
        let mut service = HashService::new(4);
        thread::sleep(Duration::from_millis(100));

        let start = Instant::now();
        service.resize_workers(0);

        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(service.count_threads(), 0);
    }

    #[test]
    fn dropping_an_idle_service_stops_its_workers_promptly() {
        let service = HashService::new(4);
        let rx = Arc::downgrade(&service.rx);
        thread::sleep(Duration::from_millis(100));

        let start = Instant::now();
        drop(service);

        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(rx.upgrade().is_none());
    }

    #[test]
    fn a_request_after_an_idle_period_starts_at_once() {
        let dir = tempdir().unwrap();
        let paths = files(dir.path(), 1);
        let service = HashService::new(2);
        thread::sleep(Duration::from_millis(200));

        let start = Instant::now();
        service.request(&paths[0]);
        while service.get_hash(&paths[0]).is_none() {
            assert!(start.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(1));
        }

        assert!(start.elapsed() < Duration::from_millis(40));
    }

    #[test]
    fn cancel_is_not_starved_by_idle_workers() {
        let service = HashService::new(4);
        thread::sleep(Duration::from_millis(100));

        let start = Instant::now();
        for _ in 0..10 {
            service.cancel();
        }

        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
