use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

/// Manages a collection of threads.
///
/// A new thread is created every time all the existing threads are full,
/// up to `max_threads`; beyond that, tasks are queued until a thread frees up.
/// Any idle thread above `min_threads` will automatically die after a few seconds.
pub struct TaskPool {
    sharing: Arc<Sharing>,
}

struct Sharing {
    // list of the tasks to be done by worker threads
    todo: Mutex<VecDeque<Box<dyn FnOnce() + Send>>>,

    // condvar that will be notified whenever a task is added to `todo`
    condvar: Condvar,

    // number of total worker threads running
    active_tasks: AtomicUsize,

    // number of idle worker threads
    waiting_tasks: AtomicUsize,

    // set by `Drop` to make idle threads exit promptly instead of waiting out their timeout
    shutdown: AtomicBool,

    // minimum number of active threads kept alive even when idle
    min_threads: usize,

    // maximum number of active threads. Once this many threads are running, further tasks
    // are queued instead of spawning new threads, so a burst of connections can't spawn one
    // OS thread per connection without bound.
    max_threads: usize,

    // maximum number of tasks waiting in the queue for a thread to become free. Once both
    // `max_threads` and this are reached, `spawn` rejects further tasks instead of growing
    // the queue without bound. Kept non-blocking (rather than making `spawn` wait for room)
    // so a saturated pool can't stall whatever thread is calling `spawn` in a loop.
    max_queue: usize,
}

struct Registration<'a> {
    nb: &'a AtomicUsize,
}

impl<'a> Registration<'a> {
    fn new(nb: &'a AtomicUsize) -> Registration<'a> {
        nb.fetch_add(1, Ordering::Release);
        Registration { nb }
    }
}

impl<'a> Drop for Registration<'a> {
    fn drop(&mut self) {
        self.nb.fetch_sub(1, Ordering::Release);
    }
}

impl TaskPool {
    /// Creates a new pool. `max_threads` and `min_threads` are clamped to at least 1 to
    /// avoid a misconfigured pool that can never spawn a thread to drain its queue.
    pub fn new(min_threads: usize, max_threads: usize, max_queue: usize) -> TaskPool {
        let max_threads = max_threads.max(1);
        let min_threads = min_threads.min(max_threads);

        let pool = TaskPool {
            sharing: Arc::new(Sharing {
                todo: Mutex::new(VecDeque::new()),
                condvar: Condvar::new(),
                active_tasks: AtomicUsize::new(0),
                waiting_tasks: AtomicUsize::new(0),
                shutdown: AtomicBool::new(false),
                min_threads,
                max_threads,
                max_queue,
            }),
        };

        for _ in 0..min_threads {
            pool.add_thread(None)
        }

        pool
    }

    /// Executes a function in a thread.
    ///
    /// If no thread is idle and the pool hasn't reached `max_threads`, spawns a new one.
    /// Otherwise the task is queued and picked up by the next thread that frees up, unless
    /// the queue is already at `max_queue`, in which case the task is dropped and `false`
    /// is returned so the caller can react (e.g. by closing the connection it came from).
    pub fn spawn(&self, code: Box<dyn FnOnce() + Send>) -> bool {
        let mut queue = self.sharing.todo.lock().unwrap();

        let no_thread_idle = self.sharing.waiting_tasks.load(Ordering::Acquire) == 0;
        let below_max =
            self.sharing.active_tasks.load(Ordering::Acquire) < self.sharing.max_threads;

        if no_thread_idle && below_max {
            self.add_thread(Some(code));
            return true;
        }

        if queue.len() >= self.sharing.max_queue {
            return false;
        }

        queue.push_back(code);
        self.sharing.condvar.notify_one();
        true
    }

    fn add_thread(&self, initial_fn: Option<Box<dyn FnOnce() + Send>>) {
        let sharing = self.sharing.clone();

        thread::spawn(move || {
            let sharing = sharing;
            let _active_guard = Registration::new(&sharing.active_tasks);

            if let Some(f) = initial_fn {
                f();
            }

            loop {
                let task: Box<dyn FnOnce() + Send> = {
                    let mut todo = sharing.todo.lock().unwrap();

                    let task;
                    loop {
                        if let Some(poped_task) = todo.pop_front() {
                            task = poped_task;
                            break;
                        }

                        if sharing.shutdown.load(Ordering::Acquire) {
                            return;
                        }

                        let _waiting_guard = Registration::new(&sharing.waiting_tasks);

                        let timed_out = if sharing.active_tasks.load(Ordering::Acquire)
                            <= sharing.min_threads
                        {
                            todo = sharing.condvar.wait(todo).unwrap();
                            false
                        } else {
                            let (new_lock, waitres) = sharing
                                .condvar
                                .wait_timeout(todo, Duration::from_millis(5000))
                                .unwrap();
                            todo = new_lock;
                            waitres.timed_out()
                        };

                        if timed_out && todo.is_empty() {
                            return;
                        }
                    }

                    task
                };

                task();
            }
        });
    }
}

impl Drop for TaskPool {
    fn drop(&mut self) {
        self.sharing.shutdown.store(true, Ordering::Release);
        self.sharing.condvar.notify_all();
    }
}
